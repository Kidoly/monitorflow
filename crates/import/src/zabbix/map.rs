//! A Zabbix configuration mapped into MonitorFlow's neutral inventory.
//!
//! Each host sees its own items, triggers and web scenarios and those of
//! its templates, linked templates included, and macros in the same
//! order. A linked template missing from the export is read from the
//! stock template of that name when Zabbix ships one, its items and
//! triggers marked approximated; otherwise it is listed as missing.

use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, NaiveTime, TimeZone, Utc};

use crate::args;
use crate::model::{
    Alert, App, AppKind, Check, Dependency, DependencyEnd, Fidelity, Host, Inventory, Origin, Os,
    Repeat, Rule, Severity, Silence, SilenceScope, Threshold, Unmovable, Watch, Window,
    EVERY_MONTH, MAX_WINDOW_MINUTES,
};
use crate::notes::Note;

use super::config::{self, Config, InterfaceKind, Item, Priority, Template, Trigger, WebScenario};
use super::items::{self, Mapped};
use super::macros::{HostValues, Macros};
use super::maintenance;
use super::media;
use super::stock;
use super::triggers::{self, Outcome};
use super::web;

/// Keys named in a built-in check's origin before "and N more".
const KEYS_SHOWN: usize = 6;
/// A maintenance description longer than this stays out of the reason.
const SHORT_DESCRIPTION: usize = 120;

/// The inventory `config` describes; `now` places recurring maintenance
/// windows.
pub fn inventory(config: &Config, now: DateTime<Utc>) -> Inventory {
    let mut out = Inventory::default();
    let mut tagged = Tagged::new();
    let mut triggers = Triggers::default();
    for host in &config.hosts {
        map_host(config, host, &mut out, &mut tagged, &mut triggers);
    }
    silences(config, now, &tagged, &mut out);
    super::services::services(config, &tagged, &mut out);
    dependencies(config, &triggers, &mut out);
    let globals = Macros::new(vec![config.global_macros.clone()], HostValues::default());
    media::channels(config, &globals, &mut out);
    super::discovery::scans(config, &mut out);
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

    fn scenario(&self, name: &str) -> Option<(String, String)> {
        let (scenario, _) = self.web.iter().find(|(w, _)| w.name == name)?;
        let first = scenario.steps.first()?.name.clone();
        let (site, _) = web::scenario(scenario, &self.macros).ok()?;
        Some((first, site.url))
    }

    fn certificate_url(&self) -> Option<String> {
        self.items.iter().find_map(|(item, _)| {
            if items::split_key(&item.key).0 != "web.certificate.get" {
                return None;
            }
            match items::map(item, &self.macros) {
                Mapped::Check(Watch::Web(site), _) => Some(site.url),
                _ => None,
            }
        })
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

/// What each host's triggers watch, with their tags, by host key: the
/// checks a maintenance limited to tags silences.
pub(super) type Tagged = BTreeMap<String, Vec<(Vec<(String, String)>, Watch)>>;

/// A check being built, with the item keys whose triggers are its alert.
struct Pending {
    check: Check,
    keys: Vec<String>,
}

fn map_host(
    config: &Config,
    host: &config::Host,
    out: &mut Inventory,
    tagged: &mut Tagged,
    mapped: &mut Triggers,
) {
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
    // Hypervisors and virtual machines Zabbix's VMware discovery made are
    // in their vCenter's inventory (ADR-0156).
    let named = |names: &[&str]| {
        view.template_names
            .iter()
            .chain(&view.missing)
            .any(|n| names.contains(&n.as_str()))
    };
    if named(&["VMware Hypervisor", "VMware Guest"]) && !named(&["VMware", "VMware FQDN"]) {
        out.unmovable.push(Unmovable {
            hosts: vec![key],
            origin: Origin::new("host", &host.host),
            why: Note::plain("vmware_discovered"),
        });
        return;
    }
    let mut addresses = Vec::new();
    // A vCenter is reached at the host of `{$VMWARE.URL}`.
    if view
        .items
        .iter()
        .any(|(item, _)| item.key.starts_with("vmware."))
    {
        if let Some(address) = view
            .macros
            .expand("{$VMWARE.URL}")
            .ok()
            .as_deref()
            .and_then(url_host)
        {
            addresses.push(address);
        }
    }
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
    // Database, Redis and Docker templates: one ready-made check each,
    // in place of their items and triggers (ADR-0183).
    let apps = app_templates(&view);
    let covered = |owner: &Owner| {
        owner.origin.kind == "template" && apps.iter().any(|(_, t)| *t == owner.origin.name)
    };
    for name in view.missing.iter().filter(|n| app_template(n).is_none()) {
        out.unmovable.push(Unmovable {
            hosts: vec![key.clone()],
            origin: Origin::new("template", name),
            why: Note::new("zabbix_template_missing", args! {"template" => name}),
        });
    }

    // Items: built-in groups by owner and measure, checks one by one.
    let mut groups: Groups = BTreeMap::new();
    let mut pending: Vec<Pending> = Vec::new();
    // No-data alerts (ADR-0153) come beside a check's own alert, as copies
    // of the check that carry only them.
    let mut silent: Vec<Check> = Vec::new();
    for (item, owner) in &view.items {
        if covered(owner) || app_item(&item.key, &apps) {
            continue;
        }
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
        // One check of its first step.
        let first_url = scenario.steps.first().map(|s| s.url.clone());
        let (site, notes) = match web::scenario(scenario, &view.macros) {
            Ok(found) => found,
            Err(why) => {
                out.unmovable.push(Unmovable {
                    hosts: vec![key.clone()],
                    origin: origin.with_detail(first_url.unwrap_or_default()),
                    why,
                });
                continue;
            }
        };
        let detail = site.url.clone();
        add(
            &mut pending,
            Pending {
                check: Check {
                    host: key.clone(),
                    watch: Watch::web(site),
                    interval_s: items::interval(scenario.delay.as_deref(), &view.macros),
                    alert: None,
                    origin: origin.with_detail(detail),
                    fidelity: fidelity(notes.into_iter().map(Some).chain([owner.note()])),
                },
                keys: vec![format!("web:{}", scenario.name)],
            },
        );
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
    for (trigger, _) in view
        .triggers
        .iter()
        .filter(|(t, owner)| !t.dependencies.is_empty() && !covered(owner))
    {
        mapped.wanting.push((key.clone(), trigger.clone()));
    }
    for (trigger, owner) in view.triggers.iter().filter(|(_, o)| !covered(o)) {
        let origin = Origin::new("trigger", &trigger.name).with_detail(&trigger.expression);
        let (severity, severity_note) = severity(trigger.priority);
        match triggers::map(trigger, &view) {
            Outcome::Thresholds(levels) => {
                for level in levels {
                    let watch = level.measure.watch(&out.checks);
                    mapped.add(&key, trigger, &watch);
                    tagged
                        .entry(key.clone())
                        .or_default()
                        .push((trigger.tags.clone(), watch));
                    let (warning, critical) = match severity {
                        Severity::Warning => (Some(level.value), None),
                        Severity::Critical => (None, Some(level.value)),
                    };
                    out.thresholds.push(Threshold {
                        judged: level.judged,
                        conditions: level.conditions,
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
                no_data,
                note,
            } => {
                let alert = Alert {
                    severity,
                    for_s,
                    no_data,
                };
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
                    mapped.add(&key, trigger, &target.check.watch);
                    tagged
                        .entry(key.clone())
                        .or_default()
                        .push((trigger.tags.clone(), target.check.watch.clone()));
                    if no_data {
                        silent.push(Check {
                            alert: Some(alert.clone()),
                            origin: origin.clone(),
                            fidelity: fidelity([note.clone(), severity_note.clone()]),
                            ..target.check.clone()
                        });
                        continue;
                    }
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
    out.checks.extend(silent);
    for (kind, template) in apps {
        let code = match kind {
            AppKind::Docker => "app_docker_template",
            AppKind::Jmx => "app_jmx_template",
            AppKind::Kubernetes => "app_kubernetes_template",
            _ => "app_template",
        };
        out.checks.push(Check {
            host: key.clone(),
            watch: Watch::app(app_of(kind, &view.macros)),
            interval_s: None,
            alert: None,
            origin: Origin::new("template", &template),
            fidelity: Fidelity::Approximated(Note::new(code, args! {"template" => template})),
        });
    }
}

/// The ready-made check a template of Zabbix's stands for: its database,
/// Redis and Docker templates read by the agent, by their names since
/// Zabbix 5.0 ("PostgreSQL by Zabbix agent 2", "Template DB MySQL").
/// Those through ODBC query from the Zabbix server with a driver: not
/// these, but for SQL Server, whose "MSSQL by ODBC" reads the counters
/// the check reads, and which the check reaches as the server did. Its
/// JMX templates, read through its Java gateway ("Generic Java JMX",
/// "Apache Tomcat by JMX", "Apache Kafka by JMX"), become the Java check,
/// which reads Jolokia instead. Its Kubernetes templates read the cluster
/// over HTTP from the Zabbix server or a proxy ("Kubernetes cluster state
/// by HTTP", "Kubernetes nodes by HTTP" and their siblings): one
/// Kubernetes check for all of them.
fn app_template(name: &str) -> Option<AppKind> {
    let name = name.to_ascii_lowercase();
    if name.contains("jmx") {
        return Some(AppKind::Jmx);
    }
    if name.contains("kubernetes") {
        return Some(AppKind::Kubernetes);
    }
    let agent = name.contains("zabbix agent")
        || name.starts_with("template db ")
        || name.starts_with("template app ");
    if name.contains("mssql") || name.contains("sql server") {
        return (agent || name.contains("odbc")).then_some(AppKind::Mssql);
    }
    if !agent || name.contains("odbc") {
        return None;
    }
    if name.contains("postgres") {
        Some(AppKind::Postgresql)
    } else if name.contains("mysql") || name.contains("mariadb") {
        Some(AppKind::Mysql)
    } else if name.contains("redis") {
        Some(AppKind::Redis)
    } else if name.contains("docker") {
        Some(AppKind::Docker)
    } else {
        None
    }
}

/// The application templates of a host, one per kind, linked ones and
/// missing ones alike: the check needs only their name.
fn app_templates(view: &View) -> Vec<(AppKind, String)> {
    let mut apps: Vec<(AppKind, String)> = Vec::new();
    for name in view.template_names.iter().chain(&view.missing) {
        if let Some(kind) = app_template(name) {
            if !apps.iter().any(|(k, _)| *k == kind) {
                apps.push((kind, name.clone()));
            }
        }
    }
    apps
}

/// Whether `key` is an item of the agent plugin of one of `apps`, which
/// the check measures already: `pgsql.ping[...]`, `docker.info`.
fn app_item(key: &str, apps: &[(AppKind, String)]) -> bool {
    let (name, _) = items::split_key(key);
    let root = name.split('.').next().unwrap_or_default();
    apps.iter().any(|(kind, _)| {
        root == match kind {
            AppKind::Postgresql => "pgsql",
            AppKind::Mysql => "mysql",
            AppKind::Mssql => "mssql",
            AppKind::Redis => "redis",
            AppKind::Jmx => "jmx",
            AppKind::Docker => "docker",
            AppKind::Kubernetes => "kube",
        }
    })
}

/// The check of `kind`, where the template's macros say it connects:
/// `{$PG.CONNSTRING.AGENT2}`, `{$MYSQL.DSN}` or `{$REDIS.CONN.URI}` of
/// agent 2 (`tcp://localhost:5432`), host and port macros of agent 1.
/// Its password, a secret macro, is never read.
fn app_of(kind: AppKind, macros: &Macros) -> App {
    let get = |name: &str| {
        macros
            .expand(name)
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty() && !v.contains('{'))
    };
    let first = |names: &[&str]| names.iter().find_map(|n| get(n));
    let mut app = App::new(kind);
    let (uri, host, port, user) = match kind {
        AppKind::Postgresql => (
            first(&["{$PG.CONNSTRING.AGENT2}", "{$PG.URI}", "{$PG.CONNSTRING}"]),
            "{$PG.HOST}",
            "{$PG.PORT}",
            "{$PG.USER}",
        ),
        AppKind::Mysql => (
            first(&["{$MYSQL.DSN}"]),
            "{$MYSQL.HOST}",
            "{$MYSQL.PORT}",
            "{$MYSQL.USER}",
        ),
        // `sqlserver://localhost:1433` of agent 2; the ODBC template
        // connects to the host's interface, at `{$MSSQL.PORT}`.
        AppKind::Mssql => (
            first(&["{$MSSQL.URI}"]),
            "",
            "{$MSSQL.PORT}",
            "{$MSSQL.USER}",
        ),
        AppKind::Redis => (first(&["{$REDIS.CONN.URI}"]), "", "", ""),
        // The host's JMX interface is the Java gateway's RMI port, not
        // Jolokia's: the check keeps Jolokia's own, 8778.
        AppKind::Jmx | AppKind::Docker => return app,
        AppKind::Kubernetes => (
            first(&[
                "{$KUBE.API.URL}",
                "{$KUBE.API.ENDPOINT.URL}",
                "{$KUBE.API.SERVER.URL}",
            ]),
            "",
            "",
            "",
        ),
    };
    // A URI with an address; a socket or a named session says none.
    if let Some(uri) = uri.filter(|u| u.contains("://") && !u.starts_with("unix")) {
        app.address = Some(crate::text::host_of(&uri)).filter(|h| !h.is_empty());
        app.port = crate::text::port_of(&uri);
    }
    if app.address.is_none() && !host.is_empty() {
        app.address = get(host);
    }
    if app.port.is_none() && !port.is_empty() {
        app.port = get(port).and_then(|p| p.parse().ok()).filter(|p| *p > 0);
    }
    // The host itself, however the macro writes it.
    app.address = app.address.filter(|a| {
        !matches!(
            a.to_ascii_lowercase().as_str(),
            "localhost" | "127.0.0.1" | "::1"
        )
    });
    if !user.is_empty() {
        app.user = get(user);
    }
    if kind == AppKind::Postgresql {
        app.database = first(&["{$PG.DATABASE}", "{$PG.DB}"]);
    }
    if kind == AppKind::Kubernetes && app.port == Some(crate::model::KUBERNETES_PORT) {
        app.port = None;
    }
    app
}

/// The host of `https://vcenter.example.com/sdk`, without its port.
fn url_host(url: &str) -> Option<String> {
    let rest = url.trim().split_once("://").map_or(url.trim(), |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority.rsplit('@').next().unwrap_or_default();
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    };
    (!host.is_empty() && !host.contains('{')).then(|| host.to_owned())
}

/// What the triggers of every host became, for their dependencies
/// (ADR-0152).
#[derive(Debug, Default)]
struct Triggers {
    /// The checks each trigger became, by (host key, trigger name).
    watches: BTreeMap<(String, String), Vec<Watch>>,
    /// (host key, trigger name) by trigger ID, from the API.
    by_id: BTreeMap<String, (String, String)>,
    /// How many triggers each check of a host has, by (host key, watch).
    per_check: BTreeMap<(String, String), usize>,
    /// Triggers that depend on others, with their host.
    wanting: Vec<(String, Trigger)>,
}

impl Triggers {
    fn add(&mut self, host: &str, trigger: &Trigger, watch: &Watch) {
        let key = (host.to_owned(), trigger.name.clone());
        if let Some(id) = &trigger.id {
            self.by_id.insert(id.clone(), key.clone());
        }
        let watches = self.watches.entry(key).or_default();
        if !watches.contains(watch) {
            watches.push(watch.clone());
            *self
                .per_check
                .entry((host.to_owned(), format!("{watch:?}")))
                .or_default() += 1;
        }
    }

    /// Whether `watch` of `host` came from one trigger only.
    fn alone(&self, host: &str, watch: &Watch) -> bool {
        self.per_check
            .get(&(host.to_owned(), format!("{watch:?}")))
            .is_none_or(|n| *n <= 1)
    }
}

/// The host a trigger expression reads, `func(/host/key)` or, before
/// Zabbix 5.4, `{host:key.func()}`.
fn expression_host(expression: &str) -> Option<String> {
    if let Some(start) = expression.find("(/") {
        let rest = &expression[start + 2..];
        return rest
            .split('/')
            .next()
            .map(str::to_owned)
            .filter(|h| !h.is_empty());
    }
    let start = expression.find('{')?;
    let rest = &expression[start + 1..];
    let host = rest.split(':').next()?;
    (!host.is_empty() && !host.contains('}')).then(|| host.to_owned())
}

/// A check a dependency names: the host as a whole when it is its agent
/// or its ping, which say whether it is down; one check otherwise.
fn parent_end(host: &str, watch: &Watch) -> DependencyEnd {
    match watch {
        Watch::Agent | Watch::Ping { address: None } => DependencyEnd::host(host),
        other => DependencyEnd::check(host, other.clone()),
    }
}

/// Trigger dependencies (ADR-0152): the check a trigger became depends on
/// the check the trigger it depends on became. A trigger of a template
/// depends on its host's copy of the other; otherwise on the host its
/// expression reads.
fn dependencies(config: &Config, triggers: &Triggers, out: &mut Inventory) {
    let mut seen: Vec<(DependencyEnd, DependencyEnd)> = Vec::new();
    for (host, trigger) in &triggers.wanting {
        let Some(children) = triggers.watches.get(&(host.clone(), trigger.name.clone())) else {
            // Not moved itself: it is listed already.
            continue;
        };
        let origin = Origin::new("trigger", &trigger.name).with_detail(&trigger.expression);
        for wanted in &trigger.dependencies {
            let parent = match &wanted.id {
                Some(id) => triggers.by_id.get(id).cloned(),
                None => {
                    let named = expression_host(&wanted.expression)
                        .filter(|h| !config.templates.contains_key(h))
                        .unwrap_or_else(|| host.clone());
                    Some((named, wanted.name.clone()))
                }
            };
            let parents = parent
                .as_ref()
                .and_then(|key| Some((key.0.clone(), triggers.watches.get(key)?)));
            let Some((parent_host, parents)) = parents else {
                out.unmovable.push(Unmovable {
                    hosts: vec![host.clone()],
                    origin: origin.clone(),
                    why: Note::new("dependency_unmapped", args! {"parent" => wanted.name}),
                });
                continue;
            };
            for child in children {
                for parent in parents {
                    let child_end = DependencyEnd::check(host.clone(), child.clone());
                    let Some(parent_end) = DependencyEnd::parent_for(
                        parent_end(&parent_host, parent),
                        parent,
                        &child_end,
                    ) else {
                        continue;
                    };
                    let pair = (child_end.clone(), parent_end.clone());
                    if seen.contains(&pair) {
                        continue;
                    }
                    seen.push(pair);
                    let exact = (child_end.watch.is_none() || triggers.alone(host, child))
                        && (parent_end.watch.is_none() || triggers.alone(&parent_host, parent));
                    out.dependencies.push(Dependency {
                        child: child_end,
                        parent: parent_end,
                        origin: origin.clone(),
                        fidelity: if exact {
                            Fidelity::Exact
                        } else {
                            Fidelity::Approximated(Note::new(
                                "dependency_whole_check",
                                args! {"trigger" => trigger.name},
                            ))
                        },
                    });
                }
            }
        }
    }
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

/// Whether a problem with `tags` is in `maintenance`: one of its tag
/// conditions passes, or with `tags_all`, one of each tag name.
fn tags_match(maintenance: &config::Maintenance, tags: &[(String, String)]) -> bool {
    let passes = |f: &config::TagFilter| tags.iter().any(|(t, v)| f.matches(t, v));
    if maintenance.tags_all {
        let names: HashSet<&str> = maintenance
            .tag_filters
            .iter()
            .map(|f| f.tag.as_str())
            .collect();
        names.iter().all(|name| {
            maintenance
                .tag_filters
                .iter()
                .filter(|f| f.tag == *name)
                .any(passes)
        })
    } else {
        maintenance.tag_filters.iter().any(passes)
    }
}

/// The rules of a recurring period, one per weekday for a monthly one by
/// week; `None` for a one-time period. The bool says it was cut to a week.
fn rules(period: &config::Period, since: i64, till: i64) -> Option<(Vec<Rule>, bool)> {
    let minutes = period.length.div_ceil(60).max(1);
    let base = Rule {
        repeat: Repeat::Weekly,
        every: 1,
        weekdays: 0,
        month_day: None,
        month_week: None,
        months: EVERY_MONTH,
        start_time: NaiveTime::from_num_seconds_from_midnight_opt(period.start_time % 86_400, 0)
            .unwrap_or_default(),
        duration_minutes: minutes.min(MAX_WINDOW_MINUTES),
        time_zone: None,
        starts_on: Some(at(since).date_naive()),
        ends_on: Some(at(till.saturating_sub(1)).date_naive()),
    };
    let every = |max: u32| u16::try_from(period.every.clamp(1, max)).unwrap_or(1);
    let weekdays = u8::try_from(period.weekdays & 0x7f).unwrap_or(0);
    let rules = match period.kind {
        config::PeriodKind::Once => return None,
        config::PeriodKind::Daily => vec![Rule {
            repeat: Repeat::Daily,
            every: every(365),
            ..base
        }],
        config::PeriodKind::Weekly => vec![Rule {
            weekdays,
            every: every(52),
            ..base
        }],
        config::PeriodKind::Monthly => {
            let months = match u16::try_from(period.months & 0xfff).unwrap_or(0) {
                0 => EVERY_MONTH,
                m => m,
            };
            let monthly = Rule {
                repeat: Repeat::Monthly,
                months,
                ..base
            };
            if period.day > 0 {
                vec![Rule {
                    month_day: u8::try_from(period.day.min(31)).ok(),
                    ..monthly
                }]
            } else {
                (0..7)
                    .filter(|bit| weekdays & (1 << bit) != 0)
                    .map(|bit| Rule {
                        weekdays: 1 << bit,
                        month_week: u8::try_from(period.every.clamp(1, 5)).ok(),
                        ..monthly.clone()
                    })
                    .collect()
            }
        }
    };
    Some((rules, minutes > MAX_WINDOW_MINUTES))
}

/// Maintenances as silences, one-time periods, and maintenance windows,
/// recurring ones, of the hosts they cover; limited to tags, of the
/// checks made from triggers with those tags.
fn silences(config: &Config, now: DateTime<Utc>, tagged: &Tagged, out: &mut Inventory) {
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
        let mut notes: Vec<Option<Note>> = Vec::new();
        if !maintenance.collect_data {
            notes.push(Some(Note::plain("zabbix_no_data")));
        }
        // Hosts that get the same checks are one silence.
        let mut parts: Vec<(Vec<String>, Vec<Watch>)> = Vec::new();
        for host in &hosts {
            let mut checks: Vec<Watch> = Vec::new();
            if !maintenance.tag_filters.is_empty() {
                for (tags, watch) in tagged.get(host).into_iter().flatten() {
                    if tags_match(maintenance, tags) && !checks.contains(watch) {
                        checks.push(watch.clone());
                    }
                }
                if checks.contains(&Watch::Agent) {
                    checks.retain(|w| *w != Watch::Agent);
                    notes.push(Some(Note::plain("window_offline_kept")));
                }
                if checks.is_empty() {
                    notes.push(Some(Note::new(
                        "zabbix_tags_unmatched",
                        args! {"tags" => maintenance.tags.join(", ")},
                    )));
                }
            }
            match parts.iter_mut().find(|(_, c)| *c == checks) {
                Some((hosts, _)) => hosts.push(host.clone()),
                None => parts.push((vec![host.clone()], checks)),
            }
        }
        if parts.is_empty() {
            parts.push((vec![], vec![]));
        }
        notes.dedup();
        let mut reason = maintenance.name.trim().to_owned();
        let description = maintenance.description.trim();
        if !description.is_empty() && description.chars().count() <= SHORT_DESCRIPTION {
            reason = format!("{reason}: {description}");
        }
        let origin = Origin::new("maintenance", &maintenance.name);
        let mut added = false;
        for period in &maintenance.periods {
            if let Some((rules, cut)) =
                rules(period, maintenance.active_since, maintenance.active_till)
            {
                let cut = cut.then(|| Note::plain("window_cut"));
                for rule in rules {
                    for (hosts, checks) in &parts {
                        out.windows.push(Window {
                            scope: SilenceScope::Hosts(hosts.clone()),
                            checks: checks.clone(),
                            rule: rule.clone(),
                            reason: reason.clone(),
                            origin: origin.clone(),
                            fidelity: fidelity(notes.iter().cloned().chain([cut.clone()])),
                        });
                        added = true;
                    }
                }
            } else if let Some((starts_at, ends_at)) = maintenance::next_window(
                period,
                maintenance.active_since,
                maintenance.active_till,
                now,
            ) {
                for (hosts, checks) in &parts {
                    out.silences.push(Silence {
                        scope: SilenceScope::Hosts(hosts.clone()),
                        checks: checks.clone(),
                        starts_at,
                        ends_at,
                        reason: reason.clone(),
                        origin: origin.clone(),
                        fidelity: fidelity(notes.iter().cloned()),
                    });
                    added = true;
                }
            }
        }
        if !added {
            // Nothing left: a window that is over, which the preview says.
            let end = at(maintenance.active_till).min(now);
            out.silences.push(Silence {
                scope: SilenceScope::Hosts(hosts),
                checks: vec![],
                starts_at: end,
                ends_at: end,
                reason,
                origin,
                fidelity: Fidelity::Exact,
            });
        }
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
