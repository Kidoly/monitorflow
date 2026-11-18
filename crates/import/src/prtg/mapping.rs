//! A PRTG [`Tree`] to an [`Inventory`]: devices become hosts with their
//! groups, sensors become watches and thresholds ([`super::sensors`]),
//! pauses and maintenance windows become silences, and notification
//! templates become channels.

use chrono::{DateTime, Duration, NaiveTime, Utc};
use monitorflow_common::redact::Secret;

use crate::args;
use crate::model::{
    Channel, Check, Fidelity, Host, Inventory, Measure, Origin, Os, Repeat, Rule, Silence,
    SilenceScope, Target, Threshold, Unmovable, Watch, Window, EVERY_MONTH, MAX_WINDOW_MINUTES,
};
use crate::notes::Note;

use super::sensors::{self, Family, Mapped};
use super::tree::{Delivery, Kind, Object, Paused, Tree};

/// Longest silence MonitorFlow keeps (ADR-0079).
const MAX_SILENCE_DAYS: i64 = 30;

fn host_key(id: i64) -> String {
    format!("device:{id}")
}

fn origin(object: &Object) -> Origin {
    let kind = match object.kind {
        Kind::Probe => "probe",
        Kind::Group => "group",
        Kind::Device => "device",
        Kind::Sensor => "sensor",
    };
    let origin = Origin::new(kind, object.name.clone());
    match &object.sensor_type {
        Some(t) => origin.with_detail(t.clone()),
        None => origin,
    }
}

/// The system of a device, from its sensors' types: WMI and Windows
/// sensors run on Windows, SSH sensors on Unix.
fn os(tree: &Tree, device: &Object) -> Os {
    let types: Vec<&str> = tree
        .children(device.id)
        .filter_map(|s| s.sensor_type.as_deref())
        .collect();
    if types
        .iter()
        .any(|t| t.starts_with("wmi") || t.starts_with("win") || t.contains("eventlog"))
    {
        Os::Windows
    } else if types.iter().any(|t| t.starts_with("ssh")) {
        Os::Linux
    } else {
        Os::Unknown
    }
}

/// A device PRTG watches over the network only, like a switch, a printer
/// or an address it pings: none of its sensors needs the machine itself
/// (WMI, SSH, its services or logs). It becomes a device (ADR-0148).
fn network_device(tree: &Tree, device: &Object) -> bool {
    tree.children(device.id).all(|sensor| {
        let t = sensor
            .sensor_type
            .as_deref()
            .unwrap_or("")
            .to_ascii_lowercase();
        match sensors::family(&t) {
            Family::Service | Family::EventLog => false,
            Family::Cpu | Family::Memory | Family::Disk | Family::Traffic => t.starts_with("snmp"),
            Family::Other => {
                !(t.starts_with("wmi") || t.starts_with("ssh") || t.starts_with("exe"))
            }
            _ => true,
        }
    })
}

/// The names of the groups above `id`, the probe's too when there are
/// several probes.
fn groups(tree: &Tree, id: i64, probes: usize) -> Vec<String> {
    let mut names = Vec::new();
    let mut current = tree
        .get(id)
        .and_then(|o| o.parent)
        .and_then(|p| tree.get(p));
    for _ in 0..tree.objects.len() {
        let Some(object) = current else { break };
        match object.kind {
            // The root group is the whole installation, not a group.
            Kind::Group if tree.ancestor(object.id, Kind::Probe).is_some() => {
                names.push(object.name.clone())
            }
            Kind::Probe if probes > 1 => names.push(object.name.clone()),
            _ => {}
        }
        current = object.parent.and_then(|p| tree.get(p));
    }
    names.reverse();
    names
}

/// Builds the inventory of `tree` at `now`.
pub fn inventory(tree: &Tree, now: DateTime<Utc>) -> Inventory {
    let mut out = Inventory::default();
    let probes = tree
        .objects
        .iter()
        .filter(|o| o.kind == Kind::Probe)
        .count();
    let mut hosts: Vec<i64> = Vec::new();
    for device in tree.objects.iter().filter(|o| o.kind == Kind::Device) {
        let agentless = network_device(tree, device) && tree.children(device.id).next().is_some();
        hosts.push(device.id);
        out.hosts.push(Host {
            key: host_key(device.id),
            name: device.name.trim().to_owned(),
            visible_name: None,
            addresses: device.host.iter().cloned().collect(),
            os: os(tree, device),
            groups: groups(tree, device.id, probes),
            origin: origin(device),
            agentless,
        });
    }
    // Certificates last: one of a site checked already goes to its check.
    let is_certificate = |o: &&Object| {
        sensors::family(o.sensor_type.as_deref().unwrap_or("")) == Family::Certificate
    };
    let sensors = tree.objects.iter().filter(|o| o.kind == Kind::Sensor);
    for sensor in sensors
        .clone()
        .filter(|o| !is_certificate(o))
        .chain(sensors.filter(is_certificate))
    {
        sensor_items(tree, sensor, &hosts, &mut out);
    }
    for object in &tree.objects {
        silences(tree, object, &hosts, now, &mut out);
    }
    channels(tree, &mut out);
    out
}

/// Where a port check connects, as the planner makes it.
fn port_target(watch: &Watch) -> Option<(String, u16)> {
    match watch {
        Watch::Port { address, port } => Some((
            address
                .as_deref()
                .unwrap_or("127.0.0.1")
                .to_ascii_lowercase(),
            *port,
        )),
        _ => None,
    }
}

/// The host and port a website check connects to.
fn site_target(url: &str) -> (String, Option<u16>) {
    (
        crate::text::host_of(url).to_ascii_lowercase(),
        crate::text::port_of(url),
    )
}

fn sensor_items(tree: &Tree, sensor: &Object, hosts: &[i64], out: &mut Inventory) {
    let Some(device) = tree.ancestor(sensor.id, Kind::Device) else {
        // Sensors of a probe or a group watch PRTG itself or the network.
        out.unmovable.push(Unmovable {
            hosts: vec![],
            origin: origin(sensor),
            why: Note::plain("prtg_not_on_device"),
        });
        return;
    };
    if !hosts.contains(&device.id) {
        // Reported once, with the device.
        return;
    }
    let key = host_key(device.id);
    let (watch, alert, fidelity, limits, lost) = match sensors::map(sensor, device.host.as_deref())
    {
        Mapped::Not(why) => {
            out.unmovable.push(Unmovable {
                hosts: vec![key],
                origin: origin(sensor),
                why,
            });
            return;
        }
        Mapped::Watch {
            watch,
            alert,
            fidelity,
            limits,
            lost,
        } => (watch, alert, fidelity, limits, lost),
    };
    let certificate =
        sensors::family(sensor.sensor_type.as_deref().unwrap_or("")) == Family::Certificate;
    let (watch, limits) = match watch {
        // A certificate of a site checked already: its limits go to that
        // check, which reads the certificate too.
        Watch::Web(site) if certificate => {
            let target = site_target(&site.url);
            let existing = out.checks.iter().find_map(|c| match &c.watch {
                Watch::Web(other) if c.host == key && site_target(&other.url) == target => {
                    Some(other.url.clone())
                }
                _ => None,
            });
            match existing {
                Some(url) => {
                    let limits = limits
                        .into_iter()
                        .map(|mut l| {
                            l.measure = Measure::CertificateDays { url: url.clone() };
                            l
                        })
                        .collect();
                    add_limits(out, &key, sensor, limits, lost);
                    return;
                }
                None => (Watch::Web(site), limits),
            }
        }
        // The same page twice is one check.
        Watch::Web(site) => {
            let taken = out.checks.iter().find_map(|c| match &c.watch {
                Watch::Web(other) if c.host == key && other.url == site.url => {
                    site_target(&other.url).1
                }
                _ => None,
            });
            if let Some(port) = taken {
                out.unmovable.push(Unmovable {
                    hosts: vec![key],
                    origin: origin(sensor),
                    why: Note::new("prtg_same_port", args! {"port" => port}),
                });
                return;
            }
            (Watch::Web(site), limits)
        }
        other => (other, limits),
    };
    // Two sensors of one port are one port check.
    if let Some(target) = port_target(&watch) {
        let taken = out
            .checks
            .iter()
            .any(|c| c.host == key && port_target(&c.watch).as_ref() == Some(&target));
        if taken {
            out.unmovable.push(Unmovable {
                hosts: vec![key],
                origin: origin(sensor),
                why: Note::new("prtg_same_port", args! {"port" => target.1}),
            });
            return;
        }
    }
    // A device's ports are checked from a probe, as PRTG did.
    let fidelity = match (&fidelity, out.host(&key).is_some_and(|h| h.agentless)) {
        (Fidelity::Approximated(n), true) if n.code == "prtg_from_server" => Fidelity::Exact,
        _ => fidelity,
    };
    out.checks.push(Check {
        host: key.clone(),
        watch,
        interval_s: sensor.interval_s,
        alert,
        origin: origin(sensor),
        fidelity,
    });
    add_limits(out, &key, sensor, limits, lost);
}

/// The thresholds of a sensor's limits, and its limits that move nowhere.
fn add_limits(
    out: &mut Inventory,
    key: &str,
    sensor: &Object,
    limits: Vec<sensors::Limit>,
    lost: Vec<String>,
) {
    let key = key.to_owned();
    for limit in limits {
        out.thresholds.push(Threshold {
            hosts: vec![key.clone()],
            measure: limit.measure,
            direction: limit.direction,
            warning: limit.warning,
            critical: limit.critical,
            for_s: 0,
            recovery: None,
            origin: Origin::new("sensor", sensor.name.clone()).with_detail(limit.channel),
            fidelity: limit.fidelity,
        });
    }
    for channel in lost {
        out.unmovable.push(Unmovable {
            hosts: vec![key.clone()],
            origin: Origin::new("sensor", sensor.name.clone()).with_detail(channel.clone()),
            why: Note::new("prtg_limit", args! {"channel" => channel}),
        });
    }
}

/// The hosts below `object`, or everything for the root.
fn scope(tree: &Tree, object: &Object, hosts: &[i64]) -> SilenceScope {
    if object.kind == Kind::Group && tree.ancestor(object.id, Kind::Probe).is_none() {
        return SilenceScope::Everything;
    }
    if let Some(device) = tree.ancestor(object.id, Kind::Device) {
        return SilenceScope::Hosts(
            hosts
                .iter()
                .filter(|h| **h == device.id)
                .map(|h| host_key(*h))
                .collect(),
        );
    }
    let covered = hosts
        .iter()
        .filter(|h| {
            let mut current = tree.get(**h);
            for _ in 0..tree.objects.len() {
                match current {
                    Some(o) if o.id == object.id => return true,
                    Some(o) => current = o.parent.and_then(|p| tree.get(p)),
                    None => return false,
                }
            }
            false
        })
        .map(|h| host_key(*h))
        .collect();
    SilenceScope::Hosts(covered)
}

fn silences(tree: &Tree, object: &Object, hosts: &[i64], now: DateTime<Utc>, out: &mut Inventory) {
    let sensor = object.kind == Kind::Sensor;
    // A device that does not move is reported once, with nothing below it.
    if tree
        .ancestor(object.id, Kind::Device)
        .is_some_and(|d| !hosts.contains(&d.id))
    {
        return;
    }
    // A sensor is one check of its device: what is silenced.
    let checks: Vec<Watch> = if sensor {
        let device = tree.ancestor(object.id, Kind::Device);
        match sensors::map(object, device.and_then(|d| d.host.as_deref())) {
            Mapped::Watch { watch, .. } if watch != Watch::Agent => vec![watch],
            _ => vec![],
        }
    } else {
        vec![]
    };
    // A sensor MonitorFlow has no check for is left out: silencing its
    // whole server would hide far more than PRTG did.
    let unmapped = sensor && checks.is_empty();
    let scheduled = object.schedule.as_ref().and_then(|name| {
        tree.schedules
            .get(name)
            .cloned()
            .flatten()
            .map(|p| (name, p))
    });
    match &object.paused {
        None => {}
        Some(Paused::ByRule) if scheduled.is_some() => {}
        Some(Paused::ByRule) => out.unmovable.push(Unmovable {
            hosts: vec![],
            origin: origin(object),
            why: Note::plain("prtg_paused_by_rule"),
        }),
        Some(_) if unmapped => out.unmovable.push(Unmovable {
            hosts: vec![],
            origin: origin(object),
            why: Note::plain("prtg_paused_sensor"),
        }),
        Some(Paused::Indefinitely) => out.silences.push(Silence {
            scope: scope(tree, object, hosts),
            checks: checks.clone(),
            starts_at: now,
            ends_at: now + Duration::days(MAX_SILENCE_DAYS),
            reason: "Paused in PRTG".into(),
            origin: origin(object),
            fidelity: Fidelity::Approximated(Note::plain("prtg_paused")),
        }),
        Some(Paused::Until(end)) => out.silences.push(Silence {
            scope: scope(tree, object, hosts),
            checks: checks.clone(),
            starts_at: now,
            ends_at: *end,
            reason: "Paused in PRTG".into(),
            origin: origin(object),
            fidelity: Fidelity::Exact,
        }),
    }
    if let Some(window) = &object.maintenance {
        if unmapped {
            out.unmovable.push(Unmovable {
                hosts: vec![],
                origin: origin(object),
                why: Note::plain("prtg_paused_sensor"),
            });
        } else {
            out.silences.push(Silence {
                scope: scope(tree, object, hosts),
                checks: checks.clone(),
                starts_at: window.start,
                ends_at: window.end,
                reason: "Maintenance window from PRTG".into(),
                origin: origin(object),
                fidelity: if tree.local_times {
                    Fidelity::Approximated(Note::plain("prtg_local_time"))
                } else {
                    Fidelity::Exact
                },
            });
        }
    }
    if let Some(name) = &object.schedule {
        let rules = scheduled
            .as_ref()
            .and_then(|(_, periods)| period_list(periods))
            .filter(|_| !unmapped);
        match rules {
            Some(rules) => {
                for rule in rules {
                    out.windows.push(Window {
                        scope: scope(tree, object, hosts),
                        checks: checks.clone(),
                        rule,
                        reason: format!("Schedule {name} from PRTG"),
                        origin: Origin::new("schedule", name.clone())
                            .with_detail(tree.path(object.id)),
                        fidelity: Fidelity::Exact,
                    });
                }
            }
            None => out.unmovable.push(Unmovable {
                hosts: vec![],
                origin: origin(object),
                why: Note::new("prtg_schedule_unknown", args! {"schedule" => name}),
            }),
        }
    }
}

/// A schedule's period list, `mo:00:00-mo:06:00` per line, as weekly
/// rules in PRTG's local time, the zone chosen for the import. `None`
/// when a line is not a period.
pub fn period_list(text: &str) -> Option<Vec<Rule>> {
    const DAYS: [&str; 7] = ["mo", "tu", "we", "th", "fr", "sa", "su"];
    let point = |p: &str| -> Option<u32> {
        let mut parts = p.trim().split(':');
        let day = parts.next()?;
        let day = DAYS.iter().position(|d| d.eq_ignore_ascii_case(day))?;
        let hours: u32 = parts.next()?.parse().ok()?;
        let minutes: u32 = parts.next()?.parse().ok()?;
        (parts.next().is_none() && hours <= 24 && minutes < 60)
            .then(|| u32::try_from(day).unwrap_or(0) * 1440 + hours * 60 + minutes)
    };
    let mut rules = Vec::new();
    for line in text
        .split(['\n', ',', ';'])
        .map(str::trim)
        .filter(|l| !l.is_empty())
    {
        let (from, to) = line.split_once('-')?;
        let (start, end) = (point(from)?, point(to)?);
        let week = 7 * 1440;
        let length = (end + week - start) % week;
        let length = if length == 0 { week } else { length };
        rules.push(Rule {
            repeat: Repeat::Weekly,
            every: 1,
            weekdays: 1 << (start / 1440),
            month_day: None,
            month_week: None,
            months: EVERY_MONTH,
            start_time: NaiveTime::from_num_seconds_from_midnight_opt((start % 1440) * 60, 0)?,
            duration_minutes: length.min(MAX_WINDOW_MINUTES),
            time_zone: None,
            starts_on: None,
            ends_on: None,
        });
    }
    (!rules.is_empty()).then_some(rules)
}

/// The target of a webhook URL, by its host.
fn http_target(url: &str) -> Target {
    let host = crate::text::host_of(url).to_ascii_lowercase();
    let url = Secret::new(url);
    if host == "hooks.slack.com" {
        Target::Slack { url }
    } else if host.ends_with(".webhook.office.com") || host.ends_with(".logic.azure.com") {
        Target::Teams { url }
    } else if host == "discord.com" || host == "discordapp.com" {
        Target::Discord { url }
    } else {
        Target::Webhook { url }
    }
}

fn channels(tree: &Tree, out: &mut Inventory) {
    for notification in &tree.notifications {
        let origin = Origin::new("notification", notification.name.clone());
        let not = |out: &mut Inventory, origin: Origin, why: Note| {
            out.unmovable.push(Unmovable {
                hosts: vec![],
                origin,
                why,
            });
        };
        if !notification.active {
            not(out, origin, Note::plain("disabled"));
            continue;
        }
        if notification.deliveries.is_empty() {
            not(out, origin, Note::plain("no_target"));
            continue;
        }
        for delivery in &notification.deliveries {
            let (target, fidelity) = match delivery {
                Delivery::Email(to) => (
                    Target::Email {
                        to: to.clone(),
                        smtp: None,
                    },
                    Fidelity::Approximated(Note::plain("prtg_triggers")),
                ),
                Delivery::Http(url) => {
                    let target = http_target(url);
                    // A webhook gets the planner's note on its message.
                    let fidelity = match target {
                        Target::Webhook { .. } => Fidelity::Exact,
                        _ => Fidelity::Approximated(Note::plain("prtg_triggers")),
                    };
                    (target, fidelity)
                }
                Delivery::Other(method) => {
                    not(
                        out,
                        origin.clone().with_detail(method.clone()),
                        Note::plain("no_target"),
                    );
                    continue;
                }
            };
            out.channels.push(Channel {
                name: notification.name.clone(),
                target,
                origin: origin.clone(),
                fidelity,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;
    use crate::prtg::tree::{Maintenance, Notification};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 17, 12, 0, 0).unwrap()
    }

    fn sensor(id: i64, parent: i64, sensor_type: &str) -> Object {
        let mut sensor = Object::new(id, Kind::Sensor, format!("sensor {id}"), Some(parent));
        sensor.sensor_type = Some(sensor_type.into());
        sensor
    }

    /// Root group 0, probes 1 and 2, group 10 under probe 1 holding the
    /// server 20 and the switch 21; server 22 under probe 2.
    fn tree() -> Tree {
        let mut server = Object::new(20, Kind::Device, "web", Some(10));
        server.host = Some("192.0.2.20".into());
        let mut port = sensor(32, 20, "port");
        port.settings.insert("port".into(), "443".into());
        let mut page = sensor(33, 20, "http");
        page.settings
            .insert("httpurl".into(), "https://192.0.2.20/".into());
        let same_page = Object {
            id: 35,
            name: "sensor 35".into(),
            ..page.clone()
        };
        // Listed before the page: certificates are read last.
        let mut certificate = sensor(36, 20, "sslcertificate");
        certificate.channels.push(super::super::tree::Channel {
            id: 0,
            name: "Days to Expiration".into(),
            unit: "#".into(),
            limits: Some(super::super::tree::Limits {
                min_warning: Some(30.0),
                min_error: Some(7.0),
                ..Default::default()
            }),
        });
        Tree {
            objects: vec![
                Object::new(0, Kind::Group, "Root", None),
                Object::new(1, Kind::Probe, "Main", Some(0)),
                Object::new(2, Kind::Probe, "Branch", Some(0)),
                Object::new(10, Kind::Group, "Web", Some(1)),
                server,
                Object::new(21, Kind::Device, "switch", Some(10)),
                Object::new(22, Kind::Device, "files", Some(2)),
                sensor(30, 20, "wmiprocessor"),
                sensor(31, 21, "snmptraffic"),
                port,
                certificate,
                page,
                sensor(34, 10, "ping"),
                same_page,
            ],
            notifications: vec![],
            local_times: false,
            schedules: Default::default(),
        }
    }

    #[test]
    fn devices_become_hosts_and_switches_devices() {
        let inv = inventory(&tree(), now());
        let hosts: Vec<(&str, &[String])> = inv
            .hosts
            .iter()
            .map(|h| (h.name.as_str(), h.groups.as_slice()))
            .collect();
        assert_eq!(hosts.len(), 3);
        // The switch is watched over SNMP only: a device (ADR-0148).
        let switch = inv.host("device:21").unwrap();
        assert!(switch.agentless);
        assert!(!inv.host("device:20").unwrap().agentless);
        assert!(inv
            .checks
            .iter()
            .any(|c| c.host == "device:21" && c.watch == Watch::Traffic));
        // Two probes: the probe is a group too.
        assert_eq!(
            hosts[0],
            ("web", &["Main".to_owned(), "Web".to_owned()][..])
        );
        assert_eq!(inv.host("device:20").unwrap().os, Os::Windows);
        let why: Vec<(&str, &str)> = inv
            .unmovable
            .iter()
            .map(|u| (u.origin.name.as_str(), u.why.code.as_str()))
            .collect();
        assert!(why.contains(&("sensor 34", "prtg_not_on_device")));
        // The port and the page are two checks; the same page twice is
        // one, and the certificate of the page's site goes to its check.
        assert!(why.contains(&("sensor 35", "prtg_same_port")));
        // The port, the page, and the switch's traffic.
        assert_eq!(inv.checks.len(), 4);
        assert!(inv
            .checks
            .iter()
            .any(|c| c.watch == Watch::web(crate::model::Website::new("https://192.0.2.20/"))));
        let days: Vec<_> = inv
            .thresholds
            .iter()
            .filter(|t| t.origin.name == "sensor 36")
            .map(|t| (&t.measure, t.direction, t.warning, t.critical))
            .collect();
        assert_eq!(
            days,
            vec![(
                &Measure::CertificateDays {
                    url: "https://192.0.2.20/".into()
                },
                crate::model::Direction::Below,
                Some(30.0),
                Some(7.0)
            )]
        );
    }

    #[test]
    fn pauses_and_windows_become_silences() {
        let mut tree = tree();
        let window = Maintenance {
            start: now() + Duration::days(1),
            end: now() + Duration::days(1) + Duration::hours(2),
        };
        tree.objects[3].maintenance = Some(window.clone());
        tree.objects[0].paused = Some(Paused::Indefinitely);
        tree.objects[7].paused = Some(Paused::Indefinitely);
        tree.objects[6].paused = Some(Paused::ByRule);
        let inv = inventory(&tree, now());
        let group = inv
            .silences
            .iter()
            .find(|s| s.origin.name == "Web")
            .unwrap();
        // The group's window covers the server and the switch.
        assert_eq!(
            group.scope,
            SilenceScope::Hosts(vec!["device:20".into(), "device:21".into()])
        );
        assert_eq!((group.starts_at, group.ends_at), (window.start, window.end));
        let root = inv
            .silences
            .iter()
            .find(|s| s.origin.name == "Root")
            .unwrap();
        assert_eq!(root.scope, SilenceScope::Everything);
        assert_eq!(root.ends_at, now() + Duration::days(30));
        let codes: Vec<(&str, &str)> = inv
            .unmovable
            .iter()
            .map(|u| (u.origin.name.as_str(), u.why.code.as_str()))
            .collect();
        // A paused sensor silences its check, when MonitorFlow has one.
        let paused = &tree.objects[7];
        match inv.silences.iter().find(|s| s.origin.name == paused.name) {
            Some(silence) => assert!(!silence.checks.is_empty(), "{silence:?}"),
            None => assert!(codes.contains(&(paused.name.as_str(), "prtg_paused_sensor"))),
        }
        assert!(codes.contains(&("files", "prtg_paused_by_rule")));
    }

    #[test]
    fn notifications_become_channels() {
        let mut tree = tree();
        let notification = |id, name: &str, active, deliveries| Notification {
            id,
            name: name.into(),
            active,
            deliveries,
        };
        tree.notifications = vec![
            notification(
                1,
                "Mail",
                true,
                vec![
                    Delivery::Email(vec!["ops@example.com".into()]),
                    Delivery::Other("SMS".into()),
                ],
            ),
            notification(
                2,
                "Teams",
                true,
                vec![Delivery::Http(
                    "https://example.webhook.office.com/x".into(),
                )],
            ),
            notification(
                3,
                "Hook",
                true,
                vec![Delivery::Http("https://hooks.example.com/x".into())],
            ),
            notification(
                4,
                "Off",
                false,
                vec![Delivery::Email(vec!["a@example.com".into()])],
            ),
            notification(5, "Empty", true, vec![]),
        ];
        let inv = inventory(&tree, now());
        let kinds: Vec<(&str, &str)> = inv
            .channels
            .iter()
            .map(|c| (c.name.as_str(), c.target.kind()))
            .collect();
        assert_eq!(
            kinds,
            [("Mail", "email"), ("Teams", "teams"), ("Hook", "webhook")]
        );
        // A webhook gets the planner's note on its message.
        assert_eq!(inv.channels[2].fidelity, Fidelity::Exact);
        let codes: Vec<(&str, &str)> = inv
            .unmovable
            .iter()
            .filter(|u| u.origin.kind == "notification")
            .map(|u| (u.origin.name.as_str(), u.why.code.as_str()))
            .collect();
        assert_eq!(
            codes,
            [
                ("Mail", "no_target"),
                ("Off", "disabled"),
                ("Empty", "no_target")
            ]
        );
    }

    #[test]
    fn schedules_become_weekly_windows() {
        let mut tree = tree();
        // The port sensor of web follows "Backups", the files device "Grid".
        tree.objects[9].schedule = Some("Backups".into());
        tree.objects[6].schedule = Some("Grid".into());
        tree.schedules.insert(
            "Backups".into(),
            Some("sa:23:00-su:02:00\nwe:12:00-we:12:30".into()),
        );
        tree.schedules.insert("Grid".into(), None);
        let inv = inventory(&tree, now());
        let windows: Vec<_> = inv
            .windows
            .iter()
            .filter(|w| w.origin.name == "Backups")
            .collect();
        assert_eq!(windows.len(), 2);
        assert_eq!(
            windows[0].scope,
            SilenceScope::Hosts(vec!["device:20".into()])
        );
        assert_eq!(
            windows[0].checks,
            vec![Watch::Port {
                address: Some("192.0.2.20".into()),
                port: 443
            }]
        );
        assert_eq!(
            (windows[0].rule.weekdays, windows[0].rule.duration_minutes),
            (1 << 5, 180)
        );
        assert_eq!(windows[0].rule.start_time.to_string(), "23:00:00");
        assert_eq!(
            (windows[1].rule.weekdays, windows[1].rule.duration_minutes),
            (1 << 2, 30)
        );
        assert!(inv
            .unmovable
            .iter()
            .any(|u| u.origin.name == "files" && u.why.code == "prtg_schedule_unknown"));
    }

    #[test]
    fn period_lists_are_read_or_refused() {
        // Over the end of the week.
        let rules = period_list("su:22:00-mo:06:00").unwrap();
        assert_eq!(
            (rules[0].weekdays, rules[0].duration_minutes),
            (1 << 6, 480)
        );
        assert!(period_list("mu:00:00-mo:01:00").is_none());
        assert!(period_list("mo:00:00").is_none());
        assert!(period_list("").is_none());
    }
}
