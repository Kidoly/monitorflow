//! Check recommendations (ADR-0134): what a server's agent saw running,
//! turned into checks of existing kinds that the server does not have
//! yet. Computed when asked, from the latest discovery report and the
//! server's effective checks; nothing here adds a check.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::discovery::Discovered;

/// Software recognised by its services and well-known ports.
struct Rule {
    key: &'static str,
    /// "PostgreSQL", in "PostgreSQL is running".
    name: &'static str,
    /// Unit stems, matched exactly or as a template (`postgresql@…`).
    units: &'static [&'static str],
    /// Ports it listens on.
    ports: &'static [u16],
    /// Whether a listening port recommends without any of the services:
    /// for ports nothing else uses, like a database in a container.
    port_alone: bool,
}

const RULES: &[Rule] = &[
    Rule {
        key: "postgresql",
        name: "PostgreSQL",
        units: &["postgresql"],
        ports: &[5432],
        port_alone: true,
    },
    Rule {
        key: "mysql",
        name: "MySQL or MariaDB",
        units: &["mysql", "mysqld", "mariadb"],
        ports: &[3306],
        port_alone: true,
    },
    Rule {
        key: "redis",
        name: "Redis",
        units: &["redis", "redis-server"],
        ports: &[6379],
        port_alone: true,
    },
    Rule {
        key: "mongodb",
        name: "MongoDB",
        units: &["mongod"],
        ports: &[27017],
        port_alone: true,
    },
    Rule {
        key: "mssql",
        name: "SQL Server",
        units: &["mssql-server"],
        ports: &[1433],
        port_alone: true,
    },
    Rule {
        key: "elasticsearch",
        name: "Elasticsearch",
        units: &["elasticsearch"],
        ports: &[9200],
        port_alone: true,
    },
    Rule {
        key: "rabbitmq",
        name: "RabbitMQ",
        units: &["rabbitmq-server"],
        ports: &[5672],
        port_alone: true,
    },
    Rule {
        key: "memcached",
        name: "Memcached",
        units: &["memcached"],
        ports: &[11211],
        port_alone: true,
    },
    Rule {
        key: "nginx",
        name: "nginx",
        units: &["nginx"],
        ports: &[80, 443],
        port_alone: false,
    },
    Rule {
        key: "apache",
        name: "Apache",
        units: &["apache2", "httpd"],
        ports: &[80, 443],
        port_alone: false,
    },
    Rule {
        key: "haproxy",
        name: "HAProxy",
        units: &["haproxy"],
        ports: &[],
        port_alone: false,
    },
    Rule {
        key: "ssh",
        name: "SSH",
        units: &["ssh", "sshd"],
        ports: &[22],
        port_alone: true,
    },
    Rule {
        key: "postfix",
        name: "Postfix",
        units: &["postfix"],
        ports: &[25],
        port_alone: false,
    },
    Rule {
        key: "dns",
        name: "A DNS server",
        units: &["named", "bind9", "unbound"],
        ports: &[53],
        port_alone: false,
    },
    Rule {
        key: "docker",
        name: "Docker",
        units: &["docker"],
        ports: &[],
        port_alone: false,
    },
    Rule {
        key: "containerd",
        name: "containerd",
        units: &["containerd"],
        ports: &[],
        port_alone: false,
    },
    Rule {
        key: "proxmox",
        name: "Proxmox VE",
        units: &["pveproxy"],
        ports: &[8006],
        port_alone: true,
    },
    Rule {
        key: "proxmox-cluster",
        name: "The Proxmox cluster service",
        units: &["pve-cluster"],
        ports: &[],
        port_alone: false,
    },
    // Last: a web server not recognised above, like one in a container.
    Rule {
        key: "web",
        name: "A web server",
        units: &[],
        ports: &[80, 443],
        port_alone: true,
    },
];

/// A check a recommendation proposes, as the API takes it: a kind and its
/// parameters. The web interface names it and adds its alert as for any
/// check added by hand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Proposed {
    pub kind: &'static str,
    pub params: Params,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Params {
    Service { units: String },
    Port { port: String, host: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Recommendation {
    /// Stable for what it is about, for dismissals: `postgresql`,
    /// `share:/mnt/backup`.
    pub key: String,
    /// "PostgreSQL is running".
    pub title: String,
    /// What the agent saw: "postgresql@16-main.service is running".
    pub evidence: Vec<String>,
    pub checks: Vec<Proposed>,
}

/// What the server checks already, from its effective configuration.
#[derive(Debug, Default)]
pub struct Covered {
    /// Unit names with their suffix, like `nginx.service`.
    pub units: BTreeSet<String>,
    /// `(host, port)` of port checks.
    pub ports: BTreeSet<(String, u16)>,
}

const LOCAL_HOSTS: &[&str] = &["127.0.0.1", "localhost", "::1", "[::1]"];

impl Covered {
    /// Adds an enabled check of `kind` with `params` (string values).
    pub fn add(&mut self, kind: &str, param: impl Fn(&str) -> Option<String>) {
        match kind {
            "systemd" => {
                for unit in param("units").unwrap_or_default().split(',') {
                    let unit = unit.trim();
                    if unit.is_empty() {
                        continue;
                    }
                    let unit = if unit.contains('.') {
                        unit.to_owned()
                    } else {
                        format!("{unit}.service")
                    };
                    self.units.insert(unit);
                }
            }
            "port" => {
                let host = param("host").unwrap_or_else(|| "127.0.0.1".into());
                let host = if LOCAL_HOSTS.contains(&host.as_str()) {
                    "127.0.0.1".into()
                } else {
                    host
                };
                if let Some(port) = param("port").and_then(|p| p.parse().ok()) {
                    self.ports.insert((host, port));
                }
            }
            _ => {}
        }
    }

    fn has(&self, check: &Proposed) -> bool {
        match &check.params {
            Params::Service { units } => self.units.contains(units),
            Params::Port { port, host } => port
                .parse()
                .is_ok_and(|p| self.ports.contains(&(host.clone(), p))),
        }
    }
}

fn stem_matches(unit: &str, stems: &[&str]) -> bool {
    let stem = unit.strip_suffix(".service").unwrap_or(unit);
    stems.iter().any(|s| {
        stem == *s
            || stem
                .strip_prefix(s)
                .is_some_and(|rest| rest.starts_with('@'))
    })
}

fn service(unit: &str) -> Proposed {
    Proposed {
        kind: "systemd",
        params: Params::Service {
            units: unit.to_owned(),
        },
    }
}

fn port(port: u16, host: &str) -> Proposed {
    Proposed {
        kind: "port",
        params: Params::Port {
            port: port.to_string(),
            host: host.to_owned(),
        },
    }
}

/// The recommendations for `found`, without the checks `covered` has, and
/// without a recommendation that has none left.
pub fn recommend(found: &Discovered, covered: &Covered) -> Vec<Recommendation> {
    let mut out = Vec::new();
    // A port proposed once, by the first rule that names it.
    let mut ports_taken = BTreeSet::new();
    for rule in RULES {
        let units: Vec<&String> = found
            .services
            .iter()
            .filter(|u| stem_matches(u, rule.units))
            .collect();
        let listening: Vec<u16> = rule
            .ports
            .iter()
            .copied()
            .filter(|p| found.ports.iter().any(|f| f.port == *p && f.local))
            .filter(|p| !ports_taken.contains(p))
            .collect();
        if units.is_empty() && (listening.is_empty() || !rule.port_alone) {
            continue;
        }
        let mut evidence: Vec<String> = units.iter().map(|u| format!("{u} is running")).collect();
        evidence.extend(listening.iter().map(|p| format!("port {p} is listening")));
        let mut checks: Vec<Proposed> = units.iter().map(|u| service(u)).collect();
        checks.extend(listening.iter().map(|p| port(*p, "127.0.0.1")));
        ports_taken.extend(listening.iter().copied());
        checks.retain(|c| !covered.has(c));
        if checks.is_empty() {
            continue;
        }
        let title = if units.is_empty() {
            format!("{} is listening", rule.name)
        } else {
            format!("{} is running", rule.name)
        };
        out.push(Recommendation {
            key: rule.key.to_owned(),
            title,
            evidence,
            checks,
        });
    }
    for mount in &found.mounts {
        let port_number = match mount.fs_type.as_str() {
            "nfs" | "nfs4" => 2049,
            "cifs" | "smb3" => 445,
            _ => continue,
        };
        if mount.source_host.is_empty() {
            continue;
        }
        let check = port(port_number, &mount.source_host);
        if covered.has(&check) {
            continue;
        }
        out.push(Recommendation {
            key: format!("share:{}", mount.path),
            title: format!("{} is a share on {}", mount.path, mount.source_host),
            evidence: vec![format!(
                "{} is mounted from {} ({})",
                mount.path, mount.source_host, mount.fs_type
            )],
            checks: vec![check],
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{Mount, Port};

    fn found(services: &[&str], ports: &[(u16, bool)]) -> Discovered {
        Discovered {
            services: services.iter().map(|s| (*s).to_owned()).collect(),
            ports: ports
                .iter()
                .map(|&(port, local)| Port {
                    port,
                    loopback_only: false,
                    local,
                })
                .collect(),
            mounts: Vec::new(),
        }
    }

    fn keys(recommendations: &[Recommendation]) -> Vec<&str> {
        recommendations.iter().map(|r| r.key.as_str()).collect()
    }

    #[test]
    fn a_running_service_and_its_port_make_one_recommendation() {
        let r = recommend(
            &found(
                &["postgresql@16-main.service", "cron.service"],
                &[(5432, true), (22, true)],
            ),
            &Covered::default(),
        );
        assert_eq!(keys(&r), ["postgresql", "ssh"]);
        assert_eq!(r[0].title, "PostgreSQL is running");
        assert_eq!(
            r[0].evidence,
            [
                "postgresql@16-main.service is running",
                "port 5432 is listening"
            ]
        );
        assert_eq!(
            r[0].checks,
            [
                service("postgresql@16-main.service"),
                port(5432, "127.0.0.1")
            ]
        );
        // SSH by its port alone, as on a socket-activated host.
        assert_eq!(r[1].title, "SSH is listening");
        assert_eq!(r[1].checks, [port(22, "127.0.0.1")]);
    }

    #[test]
    fn ports_go_to_the_first_rule_and_only_when_a_local_check_reaches_them() {
        let r = recommend(
            &found(
                &["nginx.service", "apache2.service"],
                &[(80, true), (443, false)],
            ),
            &Covered::default(),
        );
        assert_eq!(keys(&r), ["nginx", "apache"]);
        assert_eq!(
            r[0].checks,
            [service("nginx.service"), port(80, "127.0.0.1")]
        );
        assert_eq!(r[1].checks, [service("apache2.service")]);
        // Port 53 of a local resolver, with no DNS server: nothing.
        assert!(recommend(&found(&[], &[(53, true)]), &Covered::default()).is_empty());
        // An unknown web server, by its port.
        let r = recommend(&found(&[], &[(443, true)]), &Covered::default());
        assert_eq!(
            (r[0].key.as_str(), r[0].title.as_str()),
            ("web", "A web server is listening")
        );
    }

    #[test]
    fn checks_the_server_has_are_left_out() {
        let mut covered = Covered::default();
        covered.add("systemd", |k| {
            (k == "units").then(|| "nginx, redis-server".to_owned())
        });
        covered.add("port", |k| match k {
            "port" => Some("6379".into()),
            "host" => Some("localhost".into()),
            _ => None,
        });
        let r = recommend(
            &found(
                &["nginx.service", "redis-server.service"],
                &[(6379, true), (80, true)],
            ),
            &covered,
        );
        // nginx keeps its port; Redis has nothing left.
        assert_eq!(keys(&r), ["nginx"]);
        assert_eq!(r[0].checks, [port(80, "127.0.0.1")]);
    }

    #[test]
    fn network_shares_recommend_their_file_server() {
        let mut discovered = found(&[], &[]);
        discovered.mounts = vec![
            Mount {
                path: "/".into(),
                fs_type: "ext4".into(),
                source_host: String::new(),
            },
            Mount {
                path: "/mnt/backup".into(),
                fs_type: "nfs4".into(),
                source_host: "nas.lan".into(),
            },
            Mount {
                path: "/mnt/files".into(),
                fs_type: "cifs".into(),
                source_host: "files.lan".into(),
            },
        ];
        let r = recommend(&discovered, &Covered::default());
        assert_eq!(keys(&r), ["share:/mnt/backup", "share:/mnt/files"]);
        assert_eq!(r[0].title, "/mnt/backup is a share on nas.lan");
        assert_eq!(r[0].checks, [port(2049, "nas.lan")]);
        assert_eq!(r[1].checks, [port(445, "files.lan")]);
    }

    #[test]
    fn unit_names_match_exactly_or_as_templates() {
        assert!(stem_matches("redis-server.service", &["redis-server"]));
        assert!(stem_matches("postgresql@16-main.service", &["postgresql"]));
        assert!(!stem_matches("postgresql-backup.service", &["postgresql"]));
        assert!(!stem_matches("sshguard.service", &["ssh"]));
    }
}
