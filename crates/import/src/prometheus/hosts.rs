//! From scrape targets to hosts and checks.
//!
//! A node exporter target is a host. A blackbox exporter's probes are
//! checks of the host that runs the exporter. Any other exporter is a port
//! check on the host that runs it, when that host is known: MonitorFlow
//! does not collect other tools' metrics, but it can say when the
//! exporter's port stops answering. The exporters of PostgreSQL, MySQL
//! and Redis, the SQL Server exporters, cAdvisor and the JMX exporter,
//! become the ready-made check of what they read instead (ADR-0183).

use std::collections::{BTreeMap, BTreeSet};

use crate::args;
use crate::model::{App, AppKind, Check, Fidelity, Host, Inventory, Origin, Os, Unmovable, Watch};
use crate::notes::Note;
use crate::text::{host_of, port_of};

use super::blackbox;
use super::config::BlackboxModule;
use super::targets::Target;
use super::Collected;

/// Labels whose value names the machine, in order of preference.
const NAME_LABELS: &[&str] = &["hostname", "host", "nodename", "instance_name"];
/// Labels kept as groups of a host, `env=production`.
const GROUP_LABELS: &[&str] = &[
    "env",
    "environment",
    "team",
    "role",
    "group",
    "site",
    "region",
    "datacenter",
];

/// What a target is to the reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// A machine's own exporter: node exporter, or windows_exporter.
    Node(Os),
    /// A blackbox exporter probing something.
    Probe,
    /// Any other exporter.
    Exporter,
}

/// The role of `target`.
pub fn role(target: &Target) -> Role {
    if target.probe.is_some() {
        return Role::Probe;
    }
    let job = target.job.to_ascii_lowercase();
    let port = port_of(&target.address);
    if job.contains("windows") || port == Some(9182) {
        Role::Node(Os::Windows)
    } else if job.contains("node") || port == Some(9100) || target.nodename.is_some() {
        Role::Node(Os::Linux)
    } else {
        Role::Exporter
    }
}

/// What rules apply to: a target and what it became.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub job: String,
    pub instance: String,
    /// The host's key.
    pub host: String,
    /// The check it became, by index in the inventory's checks; none for
    /// a node target.
    pub check: Option<usize>,
}

/// The inventory being built, with what rules need to find their hosts
/// and checks.
#[derive(Debug, Default)]
pub struct Fleet {
    pub inventory: Inventory,
    /// Node exporter targets.
    pub nodes: Vec<Member>,
    /// Probes of blackbox exporters.
    pub probes: Vec<Member>,
    /// Other exporters, as port checks.
    pub exporters: Vec<Member>,
    /// The host running each blackbox exporter job.
    pub blackbox_hosts: Vec<Member>,
}

impl Fleet {
    /// Hosts and checks of the targets of `collected`.
    pub fn new(collected: &Collected) -> Self {
        let mut fleet = Self::default();
        let roles: Vec<(Role, &Target)> = collected.targets.iter().map(|t| (role(t), t)).collect();
        for (role, target) in &roles {
            if let Role::Node(os) = role {
                fleet.node(target, *os);
            }
        }
        for (role, target) in &roles {
            if *role == Role::Probe {
                fleet.probe(target, collected);
            }
        }
        for (role, target) in &roles {
            if *role == Role::Exporter {
                fleet.exporter(target, collected);
            }
        }
        fleet
    }

    /// The key of the host named or addressed `text`, if one is.
    pub fn find(&self, text: &str) -> Option<String> {
        let text = host_of(text).to_ascii_lowercase();
        if text.is_empty() {
            return None;
        }
        self.inventory
            .hosts
            .iter()
            .find(|h| {
                h.name.eq_ignore_ascii_case(&text)
                    || h.addresses.iter().any(|a| a.eq_ignore_ascii_case(&text))
            })
            .map(|h| h.key.clone())
    }

    /// Every name a host is known by: its name, addresses and the
    /// instances of its own targets. A probe's instance names what is
    /// probed, not the machine that probes: a silence of `db-01` must not
    /// silence the machine probing it.
    pub fn names_of(&self, key: &str) -> BTreeSet<String> {
        let mut names = BTreeSet::new();
        if let Some(host) = self.inventory.host(key) {
            names.insert(host.name.clone());
            names.extend(host.addresses.iter().cloned());
        }
        let own = self
            .nodes
            .iter()
            .chain(&self.exporters)
            .chain(&self.blackbox_hosts);
        for member in own.filter(|m| m.host == key) {
            names.insert(member.instance.clone());
        }
        names
    }

    /// The jobs of a host's targets.
    pub fn jobs_of(&self, key: &str) -> BTreeSet<String> {
        self.all_members()
            .filter(|m| m.host == key)
            .map(|m| m.job.clone())
            .collect()
    }

    fn all_members(&self) -> impl Iterator<Item = &Member> {
        self.nodes
            .iter()
            .chain(&self.probes)
            .chain(&self.exporters)
            .chain(&self.blackbox_hosts)
    }

    /// The host of `name`, made if it is not there yet, with `address`
    /// and the groups of `target`. `shown` is the name of the target it
    /// was read from.
    fn host(&mut self, name: &str, address: &str, os: Os, target: &Target, shown: &str) -> String {
        let key = name.to_ascii_lowercase();
        let mut groups = vec![target.job.clone()];
        // A probe's or another exporter's labels describe what it watches,
        // not the machine.
        let labels = matches!(role(target), Role::Node(_));
        for label in GROUP_LABELS.iter().filter(|_| labels) {
            if let Some(value) = target.labels.get(*label) {
                groups.push(format!("{label}={value}"));
            }
        }
        let address = host_of(address);
        match self.inventory.hosts.iter_mut().find(|h| h.key == key) {
            Some(host) => {
                host.os = host.os.or(os);
                if !address.is_empty() && !host.addresses.contains(&address) {
                    host.addresses.push(address);
                }
                for group in groups {
                    if !host.groups.contains(&group) {
                        host.groups.push(group);
                    }
                }
            }
            None => self.inventory.hosts.push(Host {
                key: key.clone(),
                name: name.to_owned(),
                visible_name: None,
                addresses: if address.is_empty() {
                    vec![]
                } else {
                    vec![address]
                },
                os,
                groups,
                origin: Origin::new("scrape_target", shown)
                    .with_detail(format!("job: {}", target.job)),
                agentless: false,
            }),
        }
        key
    }

    fn node(&mut self, target: &Target, os: Os) {
        let name = target
            .nodename
            .clone()
            .or_else(|| {
                NAME_LABELS
                    .iter()
                    .find_map(|l| target.labels.get(*l).filter(|v| !v.is_empty()).cloned())
            })
            .unwrap_or_else(|| host_of(target.instance()));
        let os = target
            .sysname
            .as_deref()
            .map_or(os, |s| Os::guess(s).or(os));
        let key = self.host(&name, &target.address, os, target, target.instance());
        self.nodes.push(Member {
            job: target.job.clone(),
            instance: target.instance().to_owned(),
            host: key,
            check: None,
        });
    }

    fn unmovable(&mut self, target: &Target, hosts: Vec<String>, why: Note) {
        self.inventory.unmovable.push(Unmovable {
            hosts,
            origin: Origin::new("scrape_target", target.instance())
                .with_detail(format!("job: {}", target.job)),
            why,
        });
    }

    /// A check made from a target, unless the same host has it already.
    fn add_check(&mut self, check: Check) -> usize {
        if let Some(i) = self
            .inventory
            .checks
            .iter()
            .position(|c| c.host == check.host && c.watch == check.watch)
        {
            return i;
        }
        self.inventory.checks.push(check);
        self.inventory.checks.len() - 1
    }

    fn probe(&mut self, target: &Target, collected: &Collected) {
        let Some(probe) = &target.probe else { return };
        // A ping, a DNS query or an SNMP walk is about the address probed,
        // wherever the exporter runs (ADR-0148, ADR-0149).
        let kind = match prober(&probe.module, &collected.modules) {
            _ if target.job.to_ascii_lowercase().contains("snmp") => Prober::Snmp,
            kind => kind,
        };
        if matches!(kind, Prober::Icmp | Prober::Dns | Prober::Snmp) {
            self.probed(target, probe, kind, collected);
            return;
        }
        let exporter = host_of(&target.address);
        let loopback = exporter.eq_ignore_ascii_case("localhost")
            || exporter.starts_with("127.")
            || exporter == "::1";
        if loopback || exporter.is_empty() {
            // The exporter runs beside Prometheus, whose machine the
            // configuration does not name.
            self.unmovable(target, vec![], Note::plain("unknown_host"));
            return;
        }
        let host = match self.find(&exporter) {
            Some(key) => {
                let name = self.inventory.host(&key).map(|h| h.name.clone());
                self.host(
                    &name.unwrap_or(exporter),
                    &target.address,
                    Os::Unknown,
                    target,
                    &target.address,
                )
            }
            None => self.host(
                &exporter,
                &target.address,
                Os::Unknown,
                target,
                &target.address,
            ),
        };
        if !self
            .blackbox_hosts
            .iter()
            .any(|m| m.host == host && m.job == target.job)
        {
            self.blackbox_hosts.push(Member {
                job: target.job.clone(),
                instance: target.address.clone(),
                host: host.clone(),
                check: None,
            });
        }
        let origin = Origin::new("scrape_target", &probe.target)
            .with_detail(format!("job: {}, module: {}", target.job, probe.module));
        let interval_s = collected.intervals.get(&target.job).copied();
        let make = |host: String, watch: Watch, fidelity: Fidelity| Check {
            host,
            watch,
            interval_s,
            alert: None,
            origin: origin.clone(),
            fidelity,
        };
        let module = collected.modules.get(&probe.module);
        let first = |notes: Vec<Note>| {
            notes
                .into_iter()
                .next()
                .map_or(Fidelity::Exact, Fidelity::Approximated)
        };
        let check = match prober(&probe.module, &collected.modules) {
            Prober::Tcp => {
                let port = port_of(&probe.target);
                let address = host_of(&probe.target);
                match (port, module) {
                    (Some(port), Some(module)) if !address.is_empty() && module.tcp.tls => {
                        let (site, notes) = blackbox::certificate(&address, port, module);
                        make(host.clone(), Watch::web(site), first(notes))
                    }
                    (Some(port), _) if !address.is_empty() => make(
                        host.clone(),
                        Watch::Port {
                            address: Some(address),
                            port,
                        },
                        Fidelity::Exact,
                    ),
                    _ => {
                        self.unmovable(target, vec![host], Note::plain("no_equivalent"));
                        return;
                    }
                }
            }
            Prober::Http => {
                let url = if probe.target.contains("://") {
                    probe.target.clone()
                } else {
                    format!("http://{}", probe.target)
                };
                let (site, notes) = blackbox::http(&url, module);
                make(host.clone(), Watch::web(site), first(notes))
            }
            Prober::Icmp | Prober::Dns | Prober::Snmp => return,
            Prober::Other => {
                self.unmovable(target, vec![host], Note::plain("no_equivalent"));
                return;
            }
        };
        let owner = check.host.clone();
        let index = self.add_check(check);
        self.probes.push(Member {
            job: target.job.clone(),
            instance: target.instance().to_owned(),
            host: owner,
            check: Some(index),
        });
    }

    /// The checks of an address probed: on the host of that address,
    /// which becomes a device when no node exporter runs there.
    fn probed(
        &mut self,
        target: &Target,
        probe: &super::targets::Probe,
        kind: Prober,
        collected: &Collected,
    ) {
        let subject = host_of(&probe.target);
        // A loopback address is the exporter's own machine, which no
        // probe reaches by that address.
        let loopback = subject.eq_ignore_ascii_case("localhost")
            || subject.starts_with("127.")
            || subject == "::1";
        if subject.is_empty() || loopback {
            self.unmovable(target, vec![], Note::plain("no_address"));
            return;
        }
        let key = match self.find(&probe.target) {
            Some(key) => key,
            None => {
                let key = self.host(&subject, &probe.target, Os::Unknown, target, &probe.target);
                if let Some(host) = self.inventory.hosts.iter_mut().find(|h| h.key == key) {
                    host.agentless = true;
                }
                key
            }
        };
        let origin = Origin::new("scrape_target", &probe.target)
            .with_detail(format!("job: {}, module: {}", target.job, probe.module));
        let interval_s = collected.intervals.get(&target.job).copied();
        let make = |watch: Watch, fidelity: Fidelity| Check {
            host: key.clone(),
            watch,
            interval_s,
            alert: None,
            origin: origin.clone(),
            fidelity,
        };
        let checks = match kind {
            Prober::Icmp => vec![make(Watch::Ping { address: None }, Fidelity::Exact)],
            Prober::Dns => vec![make(
                Watch::UdpPort {
                    address: None,
                    port: port_of(&probe.target).unwrap_or(53),
                    query: Some("dns".into()),
                },
                Fidelity::Approximated(Note::plain("dns_as_udp")),
            )],
            _ => {
                let mut checks = vec![make(
                    Watch::Snmp {
                        group: "system".into(),
                    },
                    Fidelity::Exact,
                )];
                let standard =
                    probe.module.to_ascii_lowercase().contains("if_mib") || probe.module.is_empty();
                if standard {
                    checks.push(make(Watch::Traffic, Fidelity::Exact));
                } else if let Some(module) = collected.snmp_modules.get(&probe.module) {
                    // A module of `snmp.yml`: its objects as custom checks
                    // (ADR-0157), each named after the module and table.
                    for (part, custom, fidelity) in super::snmp::checks(module) {
                        let mut check = make(Watch::SnmpCustom(Box::new(custom)), fidelity);
                        check.origin = Origin::new("scrape_target", &probe.module).with_detail(
                            format!("job: {}, target: {}, {part}", target.job, probe.target),
                        );
                        checks.push(check);
                    }
                }
                checks
            }
        };
        for check in checks {
            let index = self.add_check(check);
            self.probes.push(Member {
                job: target.job.clone(),
                instance: target.instance().to_owned(),
                host: key.clone(),
                check: Some(index),
            });
        }
    }

    fn exporter(&mut self, target: &Target, collected: &Collected) {
        let host = std::iter::once(target.address.as_str())
            .chain(std::iter::once(target.instance()))
            .chain(
                NAME_LABELS
                    .iter()
                    .filter_map(|l| target.labels.get(*l).map(String::as_str)),
            )
            .find_map(|text| self.find(text));
        let Some(host) = host else {
            self.unmovable(target, vec![], Note::plain("unknown_host"));
            return;
        };
        let address = host_of(&target.address);
        let scheme_default = if target.address.starts_with("https://") {
            443
        } else {
            80
        };
        let port = port_of(&target.address).unwrap_or(scheme_default);
        let name = self.inventory.host(&host).map(|h| h.name.clone());
        self.host(
            &name.unwrap_or_default(),
            &target.address,
            Os::Unknown,
            target,
            target.instance(),
        );
        let (watch, note) = match app_exporter(target) {
            Some((kind, exporter)) => {
                let mut app = App::new(kind);
                let loopback = address.eq_ignore_ascii_case("localhost")
                    || address.starts_with("127.")
                    || address == "::1";
                let note = if kind == AppKind::Docker {
                    Note::plain("app_cadvisor")
                } else if kind == AppKind::Jmx {
                    // Jolokia beside the exporter, at its own port.
                    app.address = Some(address).filter(|a| !a.is_empty() && !loopback);
                    Note::plain("app_jmx_exporter")
                } else if kind == AppKind::Kubernetes {
                    // The API server itself, scraped at its own port.
                    app.address = Some(address).filter(|a| !a.is_empty() && !loopback);
                    app.port = Some(port).filter(|p| *p != crate::model::KUBERNETES_PORT);
                    Note::new("app_kubernetes_exporter", args! {"exporter" => exporter})
                } else {
                    // The database on the exporter's machine, at its
                    // default port.
                    app.address = Some(address).filter(|a| !a.is_empty() && !loopback);
                    Note::new("app_exporter", args! {"exporter" => exporter})
                };
                (Watch::app(app), note)
            }
            None => (
                Watch::Port {
                    address: Some(address),
                    port,
                },
                Note::new("scrape_as_port", args! {"port" => port}),
            ),
        };
        let check = Check {
            host: host.clone(),
            watch,
            interval_s: collected.intervals.get(&target.job).copied(),
            alert: None,
            origin: Origin::new("scrape_target", target.instance())
                .with_detail(format!("job: {}", target.job)),
            fidelity: Fidelity::Approximated(note),
        };
        let index = self.add_check(check);
        self.exporters.push(Member {
            job: target.job.clone(),
            instance: target.instance().to_owned(),
            host,
            check: Some(index),
        });
    }
}

/// The ready-made check an exporter stands for, and its name: by the
/// default port of postgres_exporter, mysqld_exporter and
/// redis_exporter; cAdvisor, SQL Server's exporters and the JMX exporter
/// by their job's name, as their ports (8080, 4000, 9399) are any web
/// server's or any database's, or they have no port of their own; a
/// Kubernetes API server by its job's name (`kubernetes-apiservers`,
/// `apiserver`) or its port 6443.
fn app_exporter(target: &Target) -> Option<(AppKind, &'static str)> {
    let job = target.job.to_ascii_lowercase();
    if job.contains("cadvisor") {
        return Some((AppKind::Docker, "cAdvisor"));
    }
    if job.contains("mssql") || job.contains("sqlserver") || job.contains("sql_server") {
        return Some((AppKind::Mssql, "the SQL Server exporter"));
    }
    if job.contains("jmx") {
        return Some((AppKind::Jmx, "jmx_exporter"));
    }
    if job.contains("apiserver") {
        return Some((AppKind::Kubernetes, "kube-apiserver"));
    }
    match port_of(&target.address)? {
        crate::model::KUBERNETES_PORT => Some((AppKind::Kubernetes, "kube-apiserver")),
        9187 => Some((AppKind::Postgresql, "postgres_exporter")),
        9104 => Some((AppKind::Mysql, "mysqld_exporter")),
        9121 => Some((AppKind::Redis, "redis_exporter")),
        _ => None,
    }
}

/// The kind of probe a blackbox module makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prober {
    Http,
    Tcp,
    Icmp,
    /// A DNS query (ADR-0148).
    Dns,
    /// snmp_exporter's walk of a device (ADR-0149).
    Snmp,
    Other,
}

/// The prober of `module`: from `blackbox.yml` when it was uploaded,
/// otherwise guessed from the usual module names (`http_2xx`,
/// `tcp_connect`, `icmp`, `ssh_banner`).
pub fn prober(module: &str, modules: &BTreeMap<String, BlackboxModule>) -> Prober {
    let text = modules.get(module).map_or_else(
        || module.to_ascii_lowercase(),
        |m| m.prober.to_ascii_lowercase(),
    );
    let has = |words: &[&str]| words.iter().any(|w| text.contains(w));
    if has(&["icmp", "ping"]) {
        Prober::Icmp
    } else if has(&["dns"]) {
        Prober::Dns
    } else if has(&["_mib", "snmp"]) {
        Prober::Snmp
    } else if has(&["grpc", "udp"]) {
        Prober::Other
    } else if has(&["http"]) {
        Prober::Http
    } else if has(&["tcp", "ssh", "smtp", "imap", "pop3", "irc", "tls", "ldap"]) {
        Prober::Tcp
    } else {
        Prober::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(job: &str, address: &str, labels: &[(&str, &str)]) -> Target {
        let mut labels: BTreeMap<String, String> = labels
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        labels.insert("job".into(), job.into());
        Target {
            job: job.into(),
            address: address.into(),
            labels,
            ..Target::default()
        }
    }

    #[test]
    fn roles() {
        assert_eq!(role(&target("node", "a:9100", &[])), Role::Node(Os::Linux));
        assert_eq!(
            role(&target("servers", "a:9100", &[])),
            Role::Node(Os::Linux)
        );
        assert_eq!(
            role(&target("windows", "a:9182", &[])),
            Role::Node(Os::Windows)
        );
        assert_eq!(role(&target("mysql", "a:9104", &[])), Role::Exporter);
        let mut live = target("hosts", "a:9999", &[]);
        live.nodename = Some("a".into());
        assert_eq!(role(&live), Role::Node(Os::Linux));
    }

    #[test]
    fn probers() {
        let none = BTreeMap::new();
        assert_eq!(prober("http_2xx", &none), Prober::Http);
        assert_eq!(prober("tcp_connect", &none), Prober::Tcp);
        assert_eq!(prober("ssh_banner", &none), Prober::Tcp);
        assert_eq!(prober("icmp", &none), Prober::Icmp);
        assert_eq!(prober("dns_udp", &none), Prober::Dns);
        assert_eq!(prober("if_mib", &none), Prober::Snmp);
        assert_eq!(prober("grpc", &none), Prober::Other);
        let custom = BTreeMap::from([(
            "shop".to_owned(),
            BlackboxModule {
                prober: "http".into(),
                ..BlackboxModule::default()
            },
        )]);
        assert_eq!(prober("shop", &custom), Prober::Http);
        assert_eq!(prober("shop", &none), Prober::Other);
    }

    #[test]
    fn names_come_from_uname_then_labels_then_address() {
        let mut collected = Collected::default();
        let mut live = target("node", "10.0.0.5:9100", &[]);
        live.nodename = Some("web-01".into());
        live.sysname = Some("Linux".into());
        collected.targets.push(live);
        collected.targets.push(target(
            "node",
            "10.0.0.6:9100",
            &[("hostname", "db-01"), ("env", "production")],
        ));
        collected
            .targets
            .push(target("node", "cache-01.example.com:9100", &[]));
        // Another exporter on a known host merges into it.
        collected
            .targets
            .push(target("mysql", "10.0.0.6:9104", &[]));
        // An exporter on no known machine.
        collected
            .targets
            .push(target("pushgateway", "push.example.com:9091", &[]));
        collected.intervals.insert("mysql".into(), 30);
        let fleet = Fleet::new(&collected);
        let names: Vec<_> = fleet
            .inventory
            .hosts
            .iter()
            .map(|h| h.name.as_str())
            .collect();
        assert_eq!(names, vec!["web-01", "db-01", "cache-01.example.com"]);
        let db = fleet.inventory.host("db-01").unwrap();
        assert_eq!(db.addresses, vec!["10.0.0.6"]);
        assert_eq!(db.groups, vec!["node", "env=production", "mysql"]);
        assert_eq!(db.os, Os::Linux);
        // mysqld_exporter: MySQL's own check, on the exporter's machine.
        let check = &fleet.inventory.checks[0];
        assert_eq!(check.host, "db-01");
        let mut mysql = App::new(AppKind::Mysql);
        mysql.address = Some("10.0.0.6".into());
        assert_eq!(check.watch, Watch::app(mysql));
        assert_eq!(check.interval_s, Some(30));
        assert_eq!(
            check.fidelity,
            Fidelity::Approximated(Note::new(
                "app_exporter",
                args! {"exporter" => "mysqld_exporter"}
            ))
        );
        assert_eq!(fleet.inventory.unmovable.len(), 1);
        assert_eq!(fleet.inventory.unmovable[0].why.code, "unknown_host");
        assert_eq!(
            fleet.inventory.unmovable[0].origin.name,
            "push.example.com:9091"
        );
    }

    /// Exporters of databases and Redis, and cAdvisor, become the
    /// ready-made check of their host (ADR-0183); any other exporter stays
    /// a port check.
    #[test]
    fn application_exporters_become_their_checks() {
        let mut collected = Collected::default();
        collected
            .targets
            .push(target("node", "10.0.0.7:9100", &[("hostname", "app-01")]));
        for (job, address) in [
            ("postgres", "10.0.0.7:9187"),
            ("redis", "10.0.0.7:9121"),
            ("cadvisor", "10.0.0.7:8080"),
            ("web", "10.0.0.7:8080"),
            ("mssql", "10.0.0.7:4000"),
            ("kafka-jmx", "10.0.0.7:7071"),
        ] {
            collected.targets.push(target(job, address, &[]));
        }
        let fleet = Fleet::new(&collected);
        let watches: Vec<&Watch> = fleet.inventory.checks.iter().map(|c| &c.watch).collect();
        let mut pg = App::new(AppKind::Postgresql);
        pg.address = Some("10.0.0.7".into());
        let mut redis = App::new(AppKind::Redis);
        redis.address = Some("10.0.0.7".into());
        let mut mssql = App::new(AppKind::Mssql);
        mssql.address = Some("10.0.0.7".into());
        let mut jmx = App::new(AppKind::Jmx);
        jmx.address = Some("10.0.0.7".into());
        assert_eq!(
            watches,
            [
                &Watch::app(pg),
                &Watch::app(redis),
                &Watch::app(App::new(AppKind::Docker)),
                &Watch::Port {
                    address: Some("10.0.0.7".into()),
                    port: 8080
                },
                &Watch::app(mssql),
                &Watch::app(jmx),
            ]
        );
        assert!(fleet.inventory.checks.iter().all(|c| c.host == "app-01"));
        assert_eq!(
            fleet.inventory.checks[2].fidelity,
            Fidelity::Approximated(Note::plain("app_cadvisor"))
        );
        assert_eq!(
            fleet.inventory.checks[5].fidelity,
            Fidelity::Approximated(Note::plain("app_jmx_exporter"))
        );
    }

    /// A Kubernetes API server scraped by Prometheus becomes the
    /// Kubernetes check of its cluster (ADR-0183), by its job or its port.
    #[test]
    fn api_servers_become_the_kubernetes_check() {
        let mut collected = Collected::default();
        // An exporter belongs to a known host: the nodes' node_exporter.
        for (address, name) in [
            ("10.0.0.8", "cp-1"),
            ("10.0.0.9", "cp-2"),
            ("10.0.0.10", "cp-3"),
        ] {
            collected.targets.push(target(
                "node",
                &format!("{address}:9100"),
                &[("hostname", name)],
            ));
        }
        collected
            .targets
            .push(target("kubernetes-apiservers", "10.0.0.8:6443", &[]));
        collected.targets.push(target("k8s", "10.0.0.9:6443", &[]));
        collected
            .targets
            .push(target("apiserver", "10.0.0.10:16443", &[]));
        let fleet = Fleet::new(&collected);
        let checks: Vec<_> = fleet
            .inventory
            .checks
            .iter()
            .filter(|c| matches!(c.watch, Watch::App(_)))
            .collect();
        let watches: Vec<&Watch> = checks.iter().map(|c| &c.watch).collect();
        let cluster = |address: &str, port: Option<u16>| {
            let mut app = App::new(AppKind::Kubernetes);
            app.address = Some(address.into());
            app.port = port;
            Watch::app(app)
        };
        assert_eq!(
            watches,
            [
                &cluster("10.0.0.8", None),
                &cluster("10.0.0.9", None),
                &cluster("10.0.0.10", Some(16443)),
            ]
        );
        assert_eq!(
            checks[0].fidelity,
            Fidelity::Approximated(Note::new(
                "app_kubernetes_exporter",
                args! {"exporter" => "kube-apiserver"}
            ))
        );
    }
}
