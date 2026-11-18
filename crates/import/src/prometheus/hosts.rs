//! From scrape targets to hosts and checks.
//!
//! A node exporter target is a host. A blackbox exporter's probes are
//! checks of the host that runs the exporter. Any other exporter is a port
//! check on the host that runs it, when that host is known: MonitorFlow
//! does not collect other tools' metrics, but it can say when the
//! exporter's port stops answering.

use std::collections::{BTreeMap, BTreeSet};

use crate::args;
use crate::model::{Check, Fidelity, Host, Inventory, Origin, Os, Unmovable, Watch};
use crate::notes::Note;
use crate::text::{host_of, port_of};

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
        let check = match prober(&probe.module, &collected.probers) {
            Prober::Tcp => {
                let port = port_of(&probe.target);
                let address = host_of(&probe.target);
                match port {
                    Some(port) if !address.is_empty() => make(
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
                // Its module's settings are read in the next change.
                make(
                    host.clone(),
                    Watch::web(crate::model::Website::new(&url)),
                    Fidelity::Exact,
                )
            }
            Prober::Icmp => match self.find(&probe.target) {
                // Pinging a host: its agent answering says the same.
                Some(pinged) => make(
                    pinged,
                    Watch::Agent,
                    Fidelity::Approximated(Note::plain("ping_as_offline")),
                ),
                None => {
                    self.unmovable(target, vec![host], Note::plain("ping"));
                    return;
                }
            },
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
        let check = Check {
            host: host.clone(),
            watch: Watch::Port {
                address: Some(address),
                port,
            },
            interval_s: collected.intervals.get(&target.job).copied(),
            alert: None,
            origin: Origin::new("scrape_target", target.instance())
                .with_detail(format!("job: {}", target.job)),
            fidelity: Fidelity::Approximated(Note::new("scrape_as_port", args! {"port" => port})),
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

/// The kind of probe a blackbox module makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prober {
    Http,
    Tcp,
    Icmp,
    Other,
}

/// The prober of `module`: from `blackbox.yml` when it was uploaded,
/// otherwise guessed from the usual module names (`http_2xx`,
/// `tcp_connect`, `icmp`, `ssh_banner`).
pub fn prober(module: &str, probers: &BTreeMap<String, String>) -> Prober {
    let text = probers
        .get(module)
        .map_or_else(|| module.to_ascii_lowercase(), |p| p.to_ascii_lowercase());
    let has = |words: &[&str]| words.iter().any(|w| text.contains(w));
    if has(&["icmp", "ping"]) {
        Prober::Icmp
    } else if has(&["dns", "grpc", "udp"]) {
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
        assert_eq!(prober("dns_udp", &none), Prober::Other);
        let custom = BTreeMap::from([("shop".to_owned(), "http".to_owned())]);
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
        let check = &fleet.inventory.checks[0];
        assert_eq!(check.host, "db-01");
        assert_eq!(
            check.watch,
            Watch::Port {
                address: Some("10.0.0.6".into()),
                port: 9104
            }
        );
        assert_eq!(check.interval_s, Some(30));
        assert_eq!(
            check.fidelity,
            Fidelity::Approximated(Note::new("scrape_as_port", args! {"port" => 9104}))
        );
        assert_eq!(fleet.inventory.unmovable.len(), 1);
        assert_eq!(fleet.inventory.unmovable[0].why.code, "unknown_host");
        assert_eq!(
            fleet.inventory.unmovable[0].origin.name,
            "push.example.com:9091"
        );
    }
}
