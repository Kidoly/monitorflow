//! A Zabbix configuration mapped into MonitorFlow's neutral inventory.
//!
//! Each host sees its own items, triggers and web scenarios and those of
//! its templates, linked templates included, and macros in the same
//! order. A linked template missing from the export is read from the
//! stock template of that name when Zabbix ships one, its items and
//! triggers marked approximated; otherwise it is listed as missing.

use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, TimeZone, Utc};

use crate::args;
use crate::model::{
    Alert, Check, Fidelity, Host, Inventory, Origin, Os, Severity, Silence, SilenceScope,
    Threshold, Unmovable, Watch,
};
use crate::notes::Note;

use super::config::{self, Config, InterfaceKind, Item, Priority, Template, Trigger, WebScenario};
use super::items::{self, Mapped};
use super::macros::{HostValues, Macros};
use super::maintenance;
use super::media;
use super::stock;
use super::triggers::{self, Outcome};

/// Keys named in a built-in check's origin before "and N more".
const KEYS_SHOWN: usize = 6;
/// A maintenance description longer than this stays out of the reason.
const SHORT_DESCRIPTION: usize = 120;

/// The inventory `config` describes; `now` places recurring maintenance
/// windows.
pub fn inventory(config: &Config, now: DateTime<Utc>) -> Inventory {
    let mut out = Inventory::default();
    for host in &config.hosts {
        map_host(config, host, &mut out);
    }
    silences(config, now, &mut out);
    let globals = Macros::new(vec![config.global_macros.clone()], HostValues::default());
    media::channels(config, &globals, &mut out);
    merge(&mut out);
    out
}

/// Where an item or trigger comes from.
#[derive(Debug, Clone)]
struct Owner {
    /// The template or the host, as an origin.
    origin: Origin,
    /// The stock template it was read from, when the export lacked it.
    stock: Option<String>,
}

impl Owner {
    fn note(&self) -> Option<Note> {
        self.stock
            .as_ref()
            .map(|t| Note::new("zabbix_stock_template", args! {"template" => t}))
    }
}

/// One host as it sees its configuration.
struct View {
    macros: Macros,
    items: Vec<(Item, Owner)>,
    triggers: Vec<(Trigger, Owner)>,
    web: Vec<(WebScenario, Owner)>,
    /// Linked templates neither in the source nor known.
    missing: Vec<String>,
    /// Names of every template it has, for its system.
    template_names: Vec<String>,
}

impl triggers::Host for View {
    fn macros(&self) -> &Macros {
        &self.macros
    }

    /// By key as written, or with its macros expanded: the API expands
    /// them in trigger expressions.
    fn item(&self, key: &str) -> Option<&Item> {
        let items = || self.items.iter().map(|(i, _)| i);
        items()
            .find(|i| i.key == key)
            .or_else(|| items().find(|i| self.macros.expand(&i.key).is_ok_and(|k| k == key)))
    }
}

/// The templates linked to `names`, breadth first, each once, with
/// whether it was read from stock knowledge.
fn templates(config: &Config, names: &[String]) -> (Vec<(Template, Option<String>)>, Vec<String>) {
    let mut found = Vec::new();
    let mut missing = Vec::new();
    let mut seen = HashSet::new();
    let mut queue: Vec<String> = names.to_vec();
    let mut at = 0;
    while at < queue.len() {
        let name = queue[at].clone();
        at += 1;
        if !seen.insert(name.clone()) {
            continue;
        }
        if let Some(template) = config.templates.get(&name) {
            queue.extend(template.templates.iter().cloned());
            found.push((template.clone(), None));
        } else if let Some(template) = stock::template(&name) {
            queue.extend(template.templates.iter().cloned());
            found.push((template, Some(name)));
        } else if !config.inherited {
            missing.push(name);
        }
    }
    (found, missing)
}

fn view(config: &Config, host: &config::Host) -> View {
    let (linked, missing) = templates(config, &host.templates);
    let main = host
        .interfaces
        .iter()
        .find(|i| i.kind == InterfaceKind::Agent)
        .or(host.interfaces.first());
    let values = HostValues {
        host: host.host.clone(),
        name: if host.name.is_empty() {
            host.host.clone()
        } else {
            host.name.clone()
        },
        ip: main.map(|i| i.ip.clone()).unwrap_or_default(),
        dns: main.map(|i| i.dns.clone()).unwrap_or_default(),
        conn: main
            .map(|i| {
                if i.use_ip {
                    i.ip.clone()
                } else {
                    i.dns.clone()
                }
            })
            .unwrap_or_default(),
    };
    let mut levels = vec![host.macros.clone()];
    levels.extend(linked.iter().map(|(t, _)| t.macros.clone()));
    levels.push(config.global_macros.clone());
    let own = Owner {
        origin: Origin::new("host", &host.host),
        stock: None,
    };
    let owner_of = |template: &Option<String>| match template {
        Some(name) => Owner {
            origin: Origin::new("template", name),
            stock: None,
        },
        None => own.clone(),
    };
    let mut view = View {
        macros: Macros::new(levels, values),
        items: host
            .items
            .iter()
            .map(|i| (i.clone(), owner_of(&i.template)))
            .collect(),
        triggers: host
            .triggers
            .iter()
            .map(|t| (t.clone(), owner_of(&t.template)))
            .collect(),
        web: host.web.iter().map(|w| (w.clone(), own.clone())).collect(),
        missing,
        template_names: linked.iter().map(|(t, _)| t.host.clone()).collect(),
    };
    if !config.inherited {
        for (template, stock) in linked {
            let owner = Owner {
                origin: Origin::new("template", &template.host),
                stock,
            };
            let mut keys: HashSet<String> = view.items.iter().map(|(i, _)| i.key.clone()).collect();
            for item in template.items {
                if keys.insert(item.key.clone()) {
                    view.items.push((item, owner.clone()));
                }
            }
            view.triggers
                .extend(template.triggers.into_iter().map(|t| (t, owner.clone())));
            view.web
                .extend(template.web.into_iter().map(|w| (w, owner.clone())));
        }
    }
    view
}

/// MonitorFlow's severity for a priority, with a note when it changes
/// more than the name.
fn severity(priority: Priority) -> (Severity, Option<Note>) {
    let (severity, noted) = match priority {
        Priority::Information | Priority::Warning => (Severity::Warning, false),
        Priority::NotClassified | Priority::Average => (Severity::Warning, true),
        Priority::High | Priority::Disaster => (Severity::Critical, false),
    };
    let mapped = match severity {
        Severity::Warning => "Warning",
        Severity::Critical => "Critical",
    };
    let note = noted.then(|| {
        Note::new(
            "severity_mapped",
            args! {"severity" => priority.label(), "mapped" => mapped},
        )
    });
    (severity, note)
}

/// The first note of several, or exact.
fn fidelity(notes: impl IntoIterator<Item = Option<Note>>) -> Fidelity {
    notes
        .into_iter()
        .flatten()
        .next()
        .map_or(Fidelity::Exact, Fidelity::Approximated)
}

/// Keys of a built-in group, for its origin.
fn summary(keys: &[String]) -> String {
    let mut keys = keys.to_vec();
    keys.sort();
    keys.dedup();
    let shown = keys
        .iter()
        .take(KEYS_SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if keys.len() > KEYS_SHOWN {
        format!("{shown} and {} more", keys.len() - KEYS_SHOWN)
    } else {
        shown
    }
}

/// Built-in measures by (owner kind, owner name, measure): the measure,
/// its owner and the keys of its items.
type Groups = BTreeMap<(String, String, String), (Watch, Owner, Vec<String>)>;

/// A check being built, with the item keys whose triggers are its alert.
struct Pending {
    check: Check,
    keys: Vec<String>,
}

fn map_host(config: &Config, host: &config::Host, out: &mut Inventory) {
    let key = host.host.clone();
    if !host.enabled {
        out.unmovable.push(Unmovable {
            hosts: vec![key],
            origin: Origin::new("host", &host.host),
            why: Note::plain("disabled"),
        });
        return;
    }
    let view = view(config, host);
    let mut addresses = Vec::new();
    for interface in &host.interfaces {
        for address in [&interface.ip, &interface.dns] {
            let address = address.trim();
            if !address.is_empty() && !addresses.iter().any(|a| a == address) {
                addresses.push(address.to_owned());
            }
        }
    }
    let os = host
        .os
        .as_deref()
        .map(Os::guess)
        .unwrap_or_default()
        .or(Os::guess(&view.template_names.join(" ")));
    out.hosts.push(Host {
        key: key.clone(),
        name: host.host.clone(),
        visible_name: (!host.name.is_empty() && host.name != host.host).then(|| host.name.clone()),
        addresses,
        os,
        groups: host.groups.clone(),
        origin: Origin::new("host", &host.host),
        // No agent interface and no agent item: a device Zabbix pings or
        // polls over SNMP (ADR-0148).
        agentless: !host
            .interfaces
            .iter()
            .any(|i| i.kind == InterfaceKind::Agent)
            && !view
                .items
                .iter()
                .any(|(item, _)| item.kind == config::ItemKind::Agent),
    });
    for name in &view.missing {
        out.unmovable.push(Unmovable {
            hosts: vec![key.clone()],
            origin: Origin::new("template", name),
            why: Note::new("zabbix_template_missing", args! {"template" => name}),
        });
    }

    // Items: built-in groups by owner and measure, checks one by one.
    let mut groups: Groups = BTreeMap::new();
    let mut pending: Vec<Pending> = Vec::new();
    for (item, owner) in &view.items {
        let origin = Origin::new("item", &item.name).with_detail(&item.key);
        match items::map(item, &view.macros) {
            Mapped::Builtin(watch) => {
                group(&mut groups, watch, owner, &item.key);
            }
            Mapped::Agent(None) => group(&mut groups, Watch::Agent, owner, &item.key),
            Mapped::Agent(Some(note)) => add(
                &mut pending,
                Pending {
                    check: Check {
                        host: key.clone(),
                        watch: Watch::Agent,
                        interval_s: items::interval(item.delay.as_deref(), &view.macros),
                        alert: None,
                        origin,
                        fidelity: Fidelity::Approximated(note),
                    },
                    keys: keys_of(item, &view.macros),
                },
            ),
            Mapped::Check(watch, note) => add(
                &mut pending,
                Pending {
                    check: Check {
                        host: key.clone(),
                        watch,
                        interval_s: items::interval(item.delay.as_deref(), &view.macros),
                        alert: None,
                        origin,
                        fidelity: fidelity([note, owner.note()]),
                    },
                    keys: keys_of(item, &view.macros),
                },
            ),
            Mapped::Unmovable(why) => out.unmovable.push(Unmovable {
                hosts: vec![key.clone()],
                origin,
                why,
            }),
        }
    }
    for (scenario, owner) in &view.web {
        let origin = Origin::new("web_scenario", &scenario.name);
        if !scenario.enabled {
            out.unmovable.push(Unmovable {
                hosts: vec![key.clone()],
                origin,
                why: Note::plain("disabled"),
            });
            continue;
        }
        // One check per address the steps reach.
        let mut reached: Vec<(String, Option<u16>)> = Vec::new();
        for url in &scenario.urls {
            let url = match view.macros.expand(url) {
                Ok(url) => url,
                Err(name) => {
                    out.unmovable.push(Unmovable {
                        hosts: vec![key.clone()],
                        origin: origin.clone().with_detail(url),
                        why: Note::new("unresolved_macro", args! {"macro" => name}),
                    });
                    continue;
                }
            };
            let address = (crate::text::host_of(&url), crate::text::port_of(&url));
            if reached.contains(&address) {
                continue;
            }
            reached.push(address);
            add(
                &mut pending,
                Pending {
                    check: Check {
                        host: key.clone(),
                        watch: Watch::Web { url: url.clone() },
                        interval_s: items::interval(scenario.delay.as_deref(), &view.macros),
                        alert: None,
                        origin: origin.clone().with_detail(&url),
                        fidelity: fidelity([owner.note()]),
                    },
                    keys: vec![format!("web:{}", scenario.name)],
                },
            );
        }
    }
    let mut builtins: Vec<Pending> = groups
        .into_values()
        .map(|(watch, owner, keys)| Pending {
            check: Check {
                host: key.clone(),
                watch,
                interval_s: None,
                alert: None,
                origin: owner.origin.clone().with_detail(summary(&keys)),
                fidelity: fidelity([owner.note()]),
            },
            keys,
        })
        .collect();

    // Triggers: thresholds, alerts of checks, or why not.
    for (trigger, owner) in &view.triggers {
        let origin = Origin::new("trigger", &trigger.name).with_detail(&trigger.expression);
        let (severity, severity_note) = severity(trigger.priority);
        match triggers::map(trigger, &view) {
            Outcome::Thresholds(levels) => {
                for level in levels {
                    let (warning, critical) = match severity {
                        Severity::Warning => (Some(level.value), None),
                        Severity::Critical => (None, Some(level.value)),
                    };
                    out.thresholds.push(Threshold {
                        hosts: vec![key.clone()],
                        measure: level.measure,
                        direction: level.direction,
                        warning,
                        critical,
                        for_s: level.for_s,
                        recovery: level.recovery,
                        origin: origin.clone(),
                        fidelity: fidelity([level.note, owner.note(), severity_note.clone()]),
                    });
                }
            }
            Outcome::Alert {
                key: item_key,
                for_s,
                note,
            } => {
                let alert = Alert { severity, for_s };
                let targets: Vec<&mut Pending> = pending
                    .iter_mut()
                    .chain(
                        builtins
                            .iter_mut()
                            .filter(|p| p.check.watch == Watch::Agent),
                    )
                    .filter(|p| p.keys.contains(&item_key))
                    .collect();
                if targets.is_empty() {
                    out.unmovable.push(Unmovable {
                        hosts: vec![key.clone()],
                        origin,
                        why: Note::plain("expression"),
                    });
                    continue;
                }
                for target in targets {
                    let stronger = match &target.check.alert {
                        None => true,
                        Some(a) => {
                            a.severity < alert.severity
                                || (a.severity == alert.severity && alert.for_s < a.for_s)
                        }
                    };
                    if stronger {
                        target.check.alert = Some(alert.clone());
                    }
                    if target.check.fidelity == Fidelity::Exact {
                        target.check.fidelity = fidelity([note.clone(), severity_note.clone()]);
                    }
                }
            }
            Outcome::Unmovable(why) => out.unmovable.push(Unmovable {
                hosts: vec![key.clone()],
                origin,
                why,
            }),
        }
    }
    out.checks
        .extend(builtins.into_iter().chain(pending).map(|p| p.check));
}

/// An item's key as written and with its macros expanded, which is how
/// the API writes it in trigger expressions.
fn keys_of(item: &Item, macros: &Macros) -> Vec<String> {
    let mut keys = vec![item.key.clone()];
    if let Ok(expanded) = macros.expand(&item.key) {
        if expanded != item.key {
            keys.push(expanded);
        }
    }
    keys
}

/// Adds a check, or merges it into one of the same host watching the
/// same thing: two items checking one port are one check.
fn add(pending: &mut Vec<Pending>, new: Pending) {
    match pending
        .iter_mut()
        .find(|p| p.check.host == new.check.host && p.check.watch == new.check.watch)
    {
        Some(existing) => existing.keys.extend(new.keys),
        None => pending.push(new),
    }
}

fn group(groups: &mut Groups, watch: Watch, owner: &Owner, key: &str) {
    let id = (
        owner.origin.kind.clone(),
        owner.origin.name.clone(),
        format!("{watch:?}"),
    );
    groups
        .entry(id)
        .or_insert_with(|| (watch, owner.clone(), Vec::new()))
        .2
        .push(key.to_owned());
}

fn at(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(seconds, 0)
        .single()
        .unwrap_or(DateTime::<Utc>::MIN_UTC)
}

/// Maintenances as silences of the hosts they cover.
fn silences(config: &Config, now: DateTime<Utc>, out: &mut Inventory) {
    let known: HashSet<&str> = out.hosts.iter().map(|h| h.key.as_str()).collect();
    for maintenance in &config.maintenances {
        let mut hosts: Vec<String> = maintenance
            .hosts
            .iter()
            .filter(|h| known.contains(h.as_str()))
            .cloned()
            .collect();
        for host in &config.hosts {
            if known.contains(host.host.as_str())
                && host.groups.iter().any(|g| maintenance.groups.contains(g))
                && !hosts.contains(&host.host)
            {
                hosts.push(host.host.clone());
            }
        }
        let next = maintenance
            .periods
            .iter()
            .filter_map(|p| {
                maintenance::next_window(p, maintenance.active_since, maintenance.active_till, now)
                    .map(|w| (w, p.kind))
            })
            .min_by_key(|((start, _), _)| *start);
        let ((starts_at, ends_at), kind) = match next {
            Some(found) => found,
            // Nothing left: a window that is over, which the preview says.
            None => {
                let end = at(maintenance.active_till).min(now);
                ((end, end), config::PeriodKind::Once)
            }
        };
        let mut reason = maintenance.name.trim().to_owned();
        let description = maintenance.description.trim();
        if !description.is_empty() && description.chars().count() <= SHORT_DESCRIPTION {
            reason = format!("{reason}: {description}");
        }
        let format = |t: DateTime<Utc>| t.format("%Y-%m-%d %H:%M UTC").to_string();
        let note = if !maintenance.tags.is_empty() {
            Some(Note::new(
                "silence_whole_server",
                args! {"matchers" => maintenance.tags.join(", ")},
            ))
        } else if kind != config::PeriodKind::Once {
            Some(Note::new(
                "silence_next_window",
                args! {"starts" => format(starts_at), "ends" => format(ends_at)},
            ))
        } else {
            None
        };
        out.silences.push(Silence {
            scope: SilenceScope::Hosts(hosts),
            starts_at,
            ends_at,
            reason,
            origin: Origin::new("maintenance", &maintenance.name),
            fidelity: fidelity([note]),
        });
    }
}

/// One line per source item, however many hosts it concerns: unmovable
/// items and equal thresholds of several hosts are merged.
fn merge(out: &mut Inventory) {
    let mut unmovable: Vec<Unmovable> = Vec::new();
    for item in std::mem::take(&mut out.unmovable) {
        match unmovable
            .iter_mut()
            .find(|u| u.origin == item.origin && u.why == item.why)
        {
            Some(existing) => {
                for host in item.hosts {
                    if !existing.hosts.contains(&host) {
                        existing.hosts.push(host);
                    }
                }
            }
            None => unmovable.push(item),
        }
    }
    out.unmovable = unmovable;
    let mut thresholds: Vec<Threshold> = Vec::new();
    for item in std::mem::take(&mut out.thresholds) {
        let same = |t: &Threshold| {
            Threshold {
                hosts: vec![],
                ..t.clone()
            } == Threshold {
                hosts: vec![],
                ..item.clone()
            }
        };
        match thresholds.iter_mut().find(|t| same(t)) {
            Some(existing) => {
                for host in item.hosts {
                    if !existing.hosts.contains(&host) {
                        existing.hosts.push(host);
                    }
                }
            }
            None => thresholds.push(item),
        }
    }
    out.thresholds = thresholds;
}
