//! Items: what each Zabbix item key becomes in MonitorFlow.

use crate::args;
use crate::model::{Watch, Website};
use crate::notes::Note;

use super::config::{Item, ItemKind};
use super::macros::Macros;
use super::web;

/// What an item becomes.
#[derive(Debug, Clone, PartialEq)]
pub enum Mapped {
    /// Measured by every server already: processor, memory, disks, disk
    /// activity.
    Builtin(Watch),
    /// The agent answering; approximated with a note for a ping.
    Agent(Option<Note>),
    /// A check of its own, approximated when a note says how.
    Check(Watch, Option<Note>),
    Unmovable(Note),
}

/// `name[a,"b, c",d]` as its name and parameters, quotes removed.
pub fn split_key(key: &str) -> (String, Vec<String>) {
    let Some(open) = key.find('[') else {
        return (key.trim().to_owned(), vec![]);
    };
    let name = key[..open].trim().to_owned();
    let inner = key[open + 1..]
        .strip_suffix(']')
        .unwrap_or(&key[open + 1..]);
    let mut params = Vec::new();
    let mut current = String::new();
    let mut chars = inner.chars().peekable();
    let mut depth = 0;
    let mut at_start = true;
    while let Some(c) = chars.next() {
        match c {
            '"' if at_start && depth == 0 => {
                // A quoted parameter: up to the closing quote.
                while let Some(q) = chars.next() {
                    match q {
                        '\\' if chars.peek() == Some(&'"') => {
                            current.push('"');
                            chars.next();
                        }
                        '"' => break,
                        other => current.push(other),
                    }
                }
                at_start = false;
            }
            '[' => {
                depth += 1;
                current.push(c);
                at_start = false;
            }
            ']' => {
                depth -= 1;
                current.push(c);
            }
            ',' if depth == 0 => {
                params.push(std::mem::take(&mut current).trim().to_owned());
                at_start = true;
            }
            ' ' if at_start => {}
            other => {
                current.push(other);
                at_start = false;
            }
        }
    }
    params.push(current.trim().to_owned());
    (name, params)
}

/// Parameter `n`, or empty.
pub fn param(params: &[String], n: usize) -> &str {
    params.get(n).map_or("", |p| p.as_str())
}

/// The default port of a `net.tcp.service` service.
fn service_port(service: &str) -> Option<u16> {
    Some(match service {
        "ssh" => 22,
        "ftp" => 21,
        "smtp" => 25,
        "telnet" => 23,
        "http" => 80,
        "pop" => 110,
        "nntp" => 119,
        "imap" => 143,
        "ldap" => 389,
        "https" => 443,
        _ => return None,
    })
}

/// Key namespaces of Zabbix agents and their plugins: an unknown key in
/// one of them is a measure MonitorFlow has no equivalent of; any other
/// unknown key of an agent item is a UserParameter, a command.
const AGENT_NAMESPACES: &[&str] = &[
    "agent",
    "system",
    "vfs",
    "vm",
    "net",
    "proc",
    "kernel",
    "sensor",
    "zabbix",
    "web",
    "wmi",
    "perf_counter",
    "perf_counter_en",
    "registry",
    "service",
    "services",
    "eventlog",
    "hardware",
    "modbus",
    "mqtt",
    "docker",
    "mysql",
    "pgsql",
    "redis",
    "mongodb",
    "memcached",
    "oracle",
    "mssql",
    "ceph",
    "smart",
    "systemd",
    "vmware",
    "nvidia",
    "icmpping",
    "icmppingsec",
    "icmppingloss",
];

fn unmovable(code: &str) -> Mapped {
    Mapped::Unmovable(Note::plain(code))
}

/// A port, unless it is not a number.
fn port(text: &str) -> Option<u16> {
    text.trim().parse().ok().filter(|p| *p > 0)
}

/// `address` unless it means the host itself.
fn address(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty() && text != "localhost" && text != "127.0.0.1" && text != "::1")
        .then(|| text.to_owned())
}

/// What `item` becomes on a host whose macros are `macros`.
pub fn map(item: &Item, macros: &Macros) -> Mapped {
    if !item.enabled {
        return unmovable("disabled");
    }
    match item.kind {
        ItemKind::Snmp => return unmovable("snmp"),
        ItemKind::Ipmi | ItemKind::Jmx | ItemKind::Other => return unmovable("no_equivalent"),
        ItemKind::Command => return unmovable("script"),
        ItemKind::HttpAgent => {
            let Some(http) = item
                .http
                .as_deref()
                .filter(|h| !h.request.url.trim().is_empty())
            else {
                return unmovable("no_equivalent");
            };
            return match web::item(http, macros) {
                Ok((site, notes)) => Mapped::Check(Watch::web(site), notes.into_iter().next()),
                Err(note) => Mapped::Unmovable(note),
            };
        }
        _ => {}
    }
    let (name, params) = split_key(&item.key);
    let name = name.as_str();
    let builtin = match name {
        n if n.starts_with("system.cpu.") => Some(Watch::Cpu),
        n if n.starts_with("vm.memory.") || n.starts_with("system.swap.") => Some(Watch::Memory),
        n if n.starts_with("vfs.fs.") => Some(Watch::Disk),
        n if n.starts_with("vfs.dev.") => Some(Watch::DiskIo),
        _ => None,
    };
    if let Some(watch) = builtin {
        return Mapped::Builtin(watch);
    }
    match name {
        "agent.ping" => return Mapped::Agent(None),
        "zabbix" => {
            let what = param(&params, 1);
            return if param(&params, 0) == "host"
                && matches!(what, "agent" | "active_agent")
                && param(&params, 2) == "available"
            {
                Mapped::Agent(None)
            } else {
                unmovable("no_equivalent")
            };
        }
        "icmpping" | "icmppingloss" | "icmppingsec" => {
            return Mapped::Agent(Some(Note::plain("ping_as_offline")));
        }
        _ => {}
    }
    if name.starts_with("net.udp.") {
        return unmovable("udp");
    }
    if name.starts_with("net.if.") {
        return unmovable("network_traffic");
    }
    // Checks: their parameters may hold macros, which must have a value.
    let expanded = macros.expand(&item.key);
    let (_, params) = split_key(expanded.as_deref().unwrap_or(&item.key));
    let mapped = check_of(name, &params, item.kind);
    if matches!(mapped, Mapped::Check(..)) || CHECK_KEYS.contains(&name) {
        if let Err(name) = expanded {
            return Mapped::Unmovable(Note::new("unresolved_macro", args! {"macro" => name}));
        }
        if let Some(lld) = lld_macro(&params) {
            return Mapped::Unmovable(Note::new("unresolved_macro", args! {"macro" => lld}));
        }
    }
    mapped
}

/// Keys of checks MonitorFlow has.
const CHECK_KEYS: &[&str] = &[
    "net.tcp.service",
    "net.tcp.service.perf",
    "net.tcp.port",
    "net.tcp.listen",
    "systemd.unit.info",
    "systemd.unit.get",
    "systemd.unit.state",
    "systemd.unit.active",
    "service.info",
    "service_state",
    "proc.num",
    "log",
    "log.count",
    "logrt",
    "logrt.count",
    "web.page.get",
    "web.page.regexp",
    "web.page.perf",
    "web.certificate.get",
];

/// What a key that is not a measure every server has becomes, from its
/// name and its parameters with macros expanded.
fn check_of(name: &str, params: &[String], kind: ItemKind) -> Mapped {
    let p = |n| param(params, n);
    match name {
        "net.tcp.service" | "net.tcp.service.perf" => {
            let service = p(0);
            let Some(port) = port(p(2)).or_else(|| service_port(service)) else {
                return unmovable("no_equivalent");
            };
            Mapped::Check(
                Watch::Port {
                    address: address(p(1)),
                    port,
                },
                None,
            )
        }
        "net.tcp.port" => match port(p(1)) {
            Some(port) => Mapped::Check(
                Watch::Port {
                    address: address(p(0)),
                    port,
                },
                None,
            ),
            None => unmovable("no_equivalent"),
        },
        "net.tcp.listen" => match port(p(0)) {
            Some(port) => Mapped::Check(
                Watch::Port {
                    address: None,
                    port,
                },
                None,
            ),
            None => unmovable("no_equivalent"),
        },
        "systemd.unit.info" | "systemd.unit.get" | "systemd.unit.state" | "systemd.unit.active"
            if !p(0).is_empty() =>
        {
            Mapped::Check(
                Watch::Service {
                    name: p(0).to_owned(),
                },
                None,
            )
        }
        "service.info" | "service_state" if !p(0).is_empty() => Mapped::Check(
            Watch::Service {
                name: p(0).to_owned(),
            },
            None,
        ),
        "proc.num" if !p(0).is_empty() => {
            let process = p(0);
            let process = process
                .strip_suffix(".exe")
                .or_else(|| process.strip_suffix(".EXE"))
                .unwrap_or(process);
            Mapped::Check(
                Watch::Service {
                    name: process.to_owned(),
                },
                Some(Note::new("process_as_service", args! {"name" => process})),
            )
        }
        "log" | "log.count" | "logrt" | "logrt.count" => {
            let path = p(0);
            let file = path.rsplit('/').next().unwrap_or(path);
            let pattern = p(1);
            let rotating = name.starts_with("logrt");
            if path.is_empty()
                || pattern.starts_with('@')
                || (rotating && file.contains(['*', '+', '?', '[', '(', '|', '^', '$', '\\']))
            {
                return unmovable("no_equivalent");
            }
            let pattern = if pattern.is_empty() { ".*" } else { pattern };
            Mapped::Check(
                Watch::Log {
                    path: path.to_owned(),
                    pattern: pattern.to_owned(),
                },
                None,
            )
        }
        "web.page.get" | "web.page.regexp" | "web.page.perf" => {
            let host = p(0);
            if host.is_empty() {
                return unmovable("no_equivalent");
            }
            let url = if host.contains("://") {
                host.to_owned()
            } else {
                let port = port(p(2)).unwrap_or(80);
                let path = p(1).trim_start_matches('/');
                let scheme = if port == 443 { "https" } else { "http" };
                format!("{scheme}://{host}:{port}/{path}")
            };
            Mapped::Check(Watch::web(Website::new(url)), None)
        }
        // Zabbix agent 2 reads the certificate of host:port.
        "web.certificate.get" => {
            let host = p(0).trim();
            if host.is_empty() {
                return unmovable("no_equivalent");
            }
            let url = crate::web::https_url(host, port(p(1)).unwrap_or(443));
            Mapped::Check(
                Watch::web(Website::new(url).any_status()),
                Some(Note::plain("web_certificate_only")),
            )
        }
        "system.run" => unmovable("script"),
        _ => {
            let root = name.split('.').next().unwrap_or(name);
            let known = AGENT_NAMESPACES.contains(&root) || AGENT_NAMESPACES.contains(&name);
            if kind == ItemKind::Agent && !known {
                unmovable("script")
            } else {
                unmovable("no_equivalent")
            }
        }
    }
}

/// The first low-level discovery macro (`{#FSNAME}`) in `params`.
fn lld_macro(params: &[String]) -> Option<String> {
    params.iter().find_map(|p| {
        let at = p.find("{#")?;
        let end = p[at..].find('}')?;
        Some(p[at..at + end + 1].to_owned())
    })
}

/// Seconds between two collections, from a delay as written: the first
/// part of a flexible interval, macros resolved. `None` when it has none.
pub fn interval(delay: Option<&str>, macros: &Macros) -> Option<u32> {
    let delay = macros.expand(delay?).ok()?;
    let first = delay.split(';').next()?.trim();
    let seconds = crate::text::seconds(first)?;
    u32::try_from(seconds).ok().filter(|s| *s > 0)
}

#[cfg(test)]
mod tests {
    use super::super::config::{HttpAgent, Macro, WebAuth, WebRequest};
    use super::super::macros::HostValues;
    use super::*;

    fn item(key: &str, kind: ItemKind) -> Item {
        Item {
            name: key.into(),
            key: key.into(),
            kind,
            delay: Some("1m".into()),
            enabled: true,
            prototype: false,
            http: None,
            template: None,
        }
    }

    fn macros() -> Macros {
        Macros::new(
            vec![vec![
                Macro {
                    name: "{$NGINX.STUB_STATUS.HOST}".into(),
                    value: Some("localhost".into()),
                },
                Macro {
                    name: "{$NGINX.STUB_STATUS.PORT}".into(),
                    value: Some("8080".into()),
                },
                Macro {
                    name: "{$DELAY}".into(),
                    value: Some("5m".into()),
                },
            ]],
            HostValues::default(),
        )
    }

    fn agent(key: &str) -> Mapped {
        map(&item(key, ItemKind::Agent), &macros())
    }

    fn code(mapped: Mapped) -> String {
        match mapped {
            Mapped::Unmovable(note) => note.code,
            other => panic!("unmovable expected, got {other:?}"),
        }
    }

    #[test]
    fn keys_are_split() {
        assert_eq!(
            split_key(r#"net.tcp.service[http,"{$H}","8,0"]"#),
            (
                "net.tcp.service".to_owned(),
                vec!["http".into(), "{$H}".into(), "8,0".into()]
            )
        );
        assert_eq!(split_key("agent.ping"), ("agent.ping".to_owned(), vec![]));
        assert_eq!(
            split_key("system.cpu.util[,idle]"),
            ("system.cpu.util".to_owned(), vec!["".into(), "idle".into()])
        );
        assert_eq!(
            split_key(r#"vfs.fs.dependent.size["{#FSNAME}", pused]"#).1,
            vec!["{#FSNAME}".to_owned(), "pused".into()]
        );
    }

    #[test]
    fn measures_every_server_has() {
        assert_eq!(agent("system.cpu.util[,user]"), Mapped::Builtin(Watch::Cpu));
        assert_eq!(
            agent("system.cpu.load[all,avg5]"),
            Mapped::Builtin(Watch::Cpu)
        );
        assert_eq!(
            agent("vm.memory.size[available]"),
            Mapped::Builtin(Watch::Memory)
        );
        assert_eq!(
            agent("system.swap.size[,pfree]"),
            Mapped::Builtin(Watch::Memory)
        );
        assert_eq!(
            agent("vfs.fs.dependent.size[{#FSNAME},pused]"),
            Mapped::Builtin(Watch::Disk)
        );
        assert_eq!(
            agent("vfs.dev.read.await[{#DEVNAME}]"),
            Mapped::Builtin(Watch::DiskIo)
        );
        assert_eq!(agent("agent.ping"), Mapped::Agent(None));
        assert_eq!(
            map(
                &item("zabbix[host,agent,available]", ItemKind::Internal),
                &macros()
            ),
            Mapped::Agent(None)
        );
        assert_eq!(
            map(&item("icmpping", ItemKind::Simple), &macros()),
            Mapped::Agent(Some(Note::plain("ping_as_offline")))
        );
    }

    #[test]
    fn checks() {
        assert_eq!(
            agent("net.tcp.port[,5432]"),
            Mapped::Check(
                Watch::Port {
                    address: None,
                    port: 5432
                },
                None
            )
        );
        assert_eq!(
            agent("net.tcp.service[ssh]"),
            Mapped::Check(
                Watch::Port {
                    address: None,
                    port: 22
                },
                None
            )
        );
        assert_eq!(
            agent(
                r#"net.tcp.service[http,"{$NGINX.STUB_STATUS.HOST}","{$NGINX.STUB_STATUS.PORT}"]"#
            ),
            Mapped::Check(
                Watch::Port {
                    address: None,
                    port: 8080
                },
                None
            )
        );
        assert_eq!(
            agent("net.tcp.port[db.example.com,5432]"),
            Mapped::Check(
                Watch::Port {
                    address: Some("db.example.com".into()),
                    port: 5432
                },
                None
            )
        );
        assert_eq!(
            agent("systemd.unit.info[postgresql.service]"),
            Mapped::Check(
                Watch::Service {
                    name: "postgresql.service".into()
                },
                None
            )
        );
        assert_eq!(
            agent("service.info[W32Time,state]"),
            Mapped::Check(
                Watch::Service {
                    name: "W32Time".into()
                },
                None
            )
        );
        assert_eq!(
            agent("proc.num[nginx.exe]"),
            Mapped::Check(
                Watch::Service {
                    name: "nginx".into()
                },
                Some(Note::new("process_as_service", args! {"name" => "nginx"}))
            )
        );
        assert_eq!(
            agent("log[/var/log/syslog,error]"),
            Mapped::Check(
                Watch::Log {
                    path: "/var/log/syslog".into(),
                    pattern: "error".into()
                },
                None
            )
        );
        assert_eq!(
            agent("logrt[/var/log/app.log]"),
            Mapped::Check(
                Watch::Log {
                    path: "/var/log/app.log".into(),
                    pattern: ".*".into()
                },
                None
            )
        );
        assert_eq!(
            agent(
                r#"web.page.get["{$NGINX.STUB_STATUS.HOST}","basic_status","{$NGINX.STUB_STATUS.PORT}"]"#
            ),
            Mapped::Check(
                Watch::web(Website::new("http://localhost:8080/basic_status")),
                None
            )
        );
        let mut http = item("shop.health", ItemKind::HttpAgent);
        http.http = Some(Box::new(HttpAgent {
            request: WebRequest {
                url: "https://shop.example.com/health".into(),
                method: "GET".into(),
                status_codes: "200".into(),
                follow_redirects: true,
                ..WebRequest::default()
            },
            auth: WebAuth::default(),
        }));
        let mut expected = Website::new("https://shop.example.com/health");
        expected.status = Some("200".into());
        assert_eq!(
            map(&http, &macros()),
            Mapped::Check(Watch::web(expected), None)
        );
        // A PUT: approximated.
        http.http.as_mut().unwrap().request.method = "PUT".into();
        let Mapped::Check(_, note) = map(&http, &macros()) else {
            panic!()
        };
        assert_eq!(
            note,
            Some(Note::new("web_method", args! {"method" => "PUT"}))
        );
        // Without a URL there is nothing to check.
        http.http = None;
        assert_eq!(code(map(&http, &macros())), "no_equivalent");
        assert_eq!(
            agent("web.certificate.get[shop.example.com,8443]"),
            Mapped::Check(
                Watch::web(Website::new("https://shop.example.com:8443/").any_status()),
                Some(Note::plain("web_certificate_only"))
            )
        );
    }

    #[test]
    fn what_cannot_be_moved() {
        assert_eq!(code(agent("net.udp.service[ntp]")), "udp");
        assert_eq!(code(agent("net.if.in[eth0]")), "network_traffic");
        assert_eq!(code(agent("system.run[uptime]")), "script");
        assert_eq!(code(agent("backup.age[pg]")), "script");
        assert_eq!(code(agent("system.uptime")), "no_equivalent");
        assert_eq!(code(agent("logrt[/var/log/app-.*\\.log]")), "no_equivalent");
        assert_eq!(code(agent("net.tcp.port[,{$NOPE}]")), "unresolved_macro");
        assert_eq!(
            code(agent("systemd.unit.info[{#UNIT.NAME}]")),
            "unresolved_macro"
        );
        assert_eq!(
            code(map(&item("ifInOctets", ItemKind::Snmp), &macros())),
            "snmp"
        );
        assert_eq!(
            code(map(&item("nginx.version", ItemKind::Dependent), &macros())),
            "no_equivalent"
        );
        let mut off = item("net.tcp.port[,22]", ItemKind::Agent);
        off.enabled = false;
        assert_eq!(code(map(&off, &macros())), "disabled");
    }

    #[test]
    fn intervals() {
        let macros = macros();
        assert_eq!(interval(Some("30s"), &macros), Some(30));
        assert_eq!(interval(Some("{$DELAY}"), &macros), Some(300));
        assert_eq!(interval(Some("1m;50s/1-5,09:00-18:00"), &macros), Some(60));
        assert_eq!(interval(Some("0;wd1-5h9"), &macros), None);
        assert_eq!(interval(None, &macros), None);
    }
}
