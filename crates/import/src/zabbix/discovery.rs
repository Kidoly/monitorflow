//! Zabbix network discovery as MonitorFlow's network scans (ADR-0154):
//! a scan per discovery rule, with its ranges, the ports of its TCP-like
//! checks, the nearest schedule and its SNMP credentials. Discovery
//! actions that add hosts are said on the scans they act on; MonitorFlow
//! proposes and a person adds. Auto-registration cannot be moved: agents
//! connect with their site's install command (ADR-0073).

use monitorflow_common::redact::Secret;

use crate::args;
use crate::model::{Fidelity, Inventory, Origin, Scan, ScanSnmp, Unmovable};
use crate::notes::Note;
use crate::scan::{self, Ports};

use super::config::{ActionSource, Config, DiscoveryCheck, DiscoveryRule};
use super::macros::{HostValues, Macros};

/// The port a check tries when it names none; `None` for checks without
/// a TCP port (ping, SNMP) or that need one named (TCP).
fn default_port(kind: u8) -> Option<u16> {
    match kind {
        0 => Some(22),
        1 => Some(389),
        2 => Some(25),
        3 => Some(21),
        4 => Some(80),
        5 => Some(110),
        6 => Some(119),
        7 => Some(143),
        9 => Some(10050),
        14 => Some(443),
        15 => Some(23),
        _ => None,
    }
}

fn is_snmp(kind: u8) -> bool {
    matches!(kind, 10 | 11 | 13)
}

/// The SNMP credentials of `check`, or the note of why they are not
/// moved, or of how they differ.
fn snmp_of(check: &DiscoveryCheck, macros: &Macros) -> (Option<ScanSnmp>, Option<Note>) {
    let expand = |text: &str| {
        macros
            .expand(text)
            .map_err(|m| Note::new("unresolved_macro", args! {"macro" => m}))
    };
    let mut port = Ports::default();
    let port = if port.add_text(&check.ports) {
        port.kept.first().copied().unwrap_or(161)
    } else {
        161
    };
    if check.kind != 13 {
        let community = match expand(&check.community) {
            Ok(c) if !c.is_empty() => c,
            Ok(_) => return (None, None),
            Err(note) => return (None, Some(note)),
        };
        let note = (check.kind == 10).then(|| Note::plain("zabbix_snmp_old"));
        let snmp = ScanSnmp {
            version: "v2c".into(),
            port,
            community: Some(Secret::new(community)),
            ..ScanSnmp::default()
        };
        return (Some(snmp), note);
    }
    let refuse = |protocol: &str| {
        (
            None,
            Some(Note::new(
                "zabbix_snmp_protocol",
                args! {"protocol" => protocol},
            )),
        )
    };
    let user = match expand(&check.security_name) {
        Ok(u) if !u.trim().is_empty() => u.trim().to_owned(),
        Ok(_) => return (None, None),
        Err(note) => return (None, Some(note)),
    };
    let mut snmp = ScanSnmp {
        version: "v3".into(),
        port,
        user: Some(user),
        ..ScanSnmp::default()
    };
    if check.security_level >= 1 {
        snmp.auth_protocol = Some(
            match check.auth_protocol {
                0 => "md5",
                1 => "sha",
                3 => "sha256",
                4 => "sha384",
                5 => "sha512",
                _ => return refuse("SHA-224"),
            }
            .into(),
        );
        match expand(&check.auth_passphrase) {
            Ok(p) => snmp.auth_password = Some(Secret::new(p)),
            Err(note) => return (None, Some(note)),
        }
    }
    if check.security_level >= 2 {
        snmp.priv_protocol = Some(
            match check.priv_protocol {
                1 => "aes128",
                3 => "aes256",
                5 => "aes256c",
                0 => return refuse("DES"),
                _ => return refuse("AES-192"),
            }
            .into(),
        );
        match expand(&check.priv_passphrase) {
            Ok(p) => snmp.priv_password = Some(Secret::new(p)),
            Err(note) => return (None, Some(note)),
        }
    }
    (Some(snmp), None)
}

fn scan_of(config: &Config, rule: &DiscoveryRule, macros: &Macros) -> Result<Scan, Note> {
    let entries: Vec<&str> = rule.iprange.split(',').collect();
    let targets = scan::targets(&entries);
    if targets.entries.is_empty() {
        return Err(Note::plain("no_address"));
    }
    let mut notes: Vec<Option<Note>> = vec![targets.note()];
    let every = if rule.enabled {
        match macros.expand(&rule.delay) {
            Ok(delay) => match crate::text::seconds(&delay) {
                Some(seconds) => {
                    let (every, note) = scan::every(seconds, &delay);
                    notes.push(note);
                    Some(every)
                }
                None => None,
            },
            Err(m) => {
                notes.push(Some(Note::new("unresolved_macro", args! {"macro" => m})));
                None
            }
        }
    } else {
        notes.insert(0, Some(Note::plain("scan_disabled")));
        None
    };
    let adding = config.actions.iter().find(|a| {
        a.source == ActionSource::Discovery
            && a.enabled
            && a.adds_hosts
            && (a.rules.is_empty() || a.rules.contains(&rule.id))
    });
    if let Some(action) = adding {
        notes.push(Some(Note::new(
            "scan_proposes",
            args! {"action" => action.name},
        )));
    }
    let mut ports = Ports::default();
    for check in &rule.checks {
        if is_snmp(check.kind) {
            continue;
        }
        if check.ports.trim().is_empty() || !ports.add_text(&check.ports) {
            if let Some(port) = default_port(check.kind) {
                ports.add(port, port);
            }
        }
    }
    notes.push(ports.note());
    let mut snmp_checks = rule.checks.iter().filter(|c| is_snmp(c.kind));
    let snmp = snmp_checks.next().and_then(|check| {
        let (snmp, note) = snmp_of(check, macros);
        notes.push(note);
        snmp
    });
    if snmp.is_some() && snmp_checks.next().is_some() {
        notes.push(Some(Note::plain("zabbix_snmp_first")));
    }
    Ok(Scan {
        name: rule.name.trim().to_owned(),
        targets: targets.entries,
        ports: Some(ports.kept),
        every,
        snmp,
        group: rule.proxy.clone(),
        origin: Origin::new("discovery_rule", &rule.name).with_detail(&rule.iprange),
        fidelity: notes
            .into_iter()
            .flatten()
            .next()
            .map_or(Fidelity::Exact, Fidelity::Approximated),
    })
}

/// The scans of `config`'s discovery rules, and what of its actions
/// cannot be moved.
pub fn scans(config: &Config, out: &mut Inventory) {
    let macros = Macros::new(vec![config.global_macros.clone()], HostValues::default());
    for rule in &config.discovery {
        match scan_of(config, rule, &macros) {
            Ok(scan) => out.scans.push(scan),
            Err(why) => out.unmovable.push(Unmovable {
                hosts: vec![],
                origin: Origin::new("discovery_rule", &rule.name).with_detail(&rule.iprange),
                why,
            }),
        }
    }
    for action in &config.actions {
        if action.source == ActionSource::Registration {
            out.unmovable.push(Unmovable {
                hosts: vec![],
                origin: Origin::new("action", &action.name),
                why: Note::plain("zabbix_autoregistration"),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::config::{Action, Macro};
    use super::*;
    use crate::model::Every;

    fn check(kind: u8, ports: &str) -> DiscoveryCheck {
        DiscoveryCheck {
            kind,
            ports: ports.into(),
            ..DiscoveryCheck::default()
        }
    }

    fn rule(iprange: &str, delay: &str, checks: Vec<DiscoveryCheck>) -> DiscoveryRule {
        DiscoveryRule {
            id: "1".into(),
            name: "Office".into(),
            iprange: iprange.into(),
            delay: delay.into(),
            enabled: true,
            proxy: None,
            checks,
        }
    }

    fn read(config: &Config) -> Inventory {
        let mut out = Inventory::default();
        scans(config, &mut out);
        out
    }

    fn note(scan: &Scan) -> &str {
        match &scan.fidelity {
            Fidelity::Approximated(n) => &n.code,
            Fidelity::Exact => "exact",
        }
    }

    #[test]
    fn a_rule_becomes_a_scan() {
        let config = Config {
            discovery: vec![rule(
                "192.168.1.1-254, 10.0.0.0/30",
                "1h",
                vec![
                    check(0, "22"),
                    check(4, "80"),
                    check(8, "8000-8002,80"),
                    check(12, ""),
                ],
            )],
            ..Config::default()
        };
        let scan = &read(&config).scans[0];
        assert_eq!(scan.targets, ["192.168.1.1-254", "10.0.0.0/30"]);
        assert_eq!(scan.ports.as_deref(), Some(&[22, 80, 8000, 8001, 8002][..]));
        assert_eq!(scan.every, Some(Every::Hour));
        assert_eq!(note(scan), "exact");
        assert_eq!(scan.origin.kind, "discovery_rule");
    }

    #[test]
    fn what_differs_says_so() {
        let mut disabled = rule("10.0.0.1", "1h", vec![check(9, "")]);
        disabled.enabled = false;
        let config = Config {
            discovery: vec![
                rule("10.0.0.1", "{$DELAY}", vec![]),
                rule("10.0.0.1", "2h", vec![]),
                disabled,
                rule("10.0.0.0/16", "1d", vec![]),
                rule("router.example.com", "1d", vec![]),
            ],
            global_macros: vec![Macro {
                name: "{$DELAY}".into(),
                value: Some("1w".into()),
            }],
            actions: vec![Action {
                name: "Add Linux servers".into(),
                source: ActionSource::Discovery,
                enabled: true,
                rules: vec!["2".into()],
                adds_hosts: true,
            }],
            ..Config::default()
        };
        let out = read(&config);
        assert_eq!(
            (out.scans[0].every, note(&out.scans[0])),
            (Some(Every::Week), "exact")
        );
        assert_eq!(note(&out.scans[1]), "scan_every_hour");
        assert_eq!(
            (out.scans[2].every, note(&out.scans[2])),
            (None, "scan_disabled")
        );
        assert_eq!(out.scans[2].ports.as_deref(), Some(&[10050][..]));
        assert_eq!(note(&out.scans[3]), "scan_cut");
        // A name only: nothing to scan.
        assert_eq!(out.scans.len(), 4);
        assert_eq!(out.unmovable[0].why.code, "no_address");

        // The action limited to another rule says nothing here; one on
        // every rule says it on each.
        let mut config = config;
        config.actions[0].rules.clear();
        config.discovery.truncate(1);
        let scan = &read(&config).scans[0];
        assert!(matches!(&scan.fidelity, Fidelity::Approximated(n)
            if n.code == "scan_proposes" && n.args["action"] == "Add Linux servers"));
    }

    #[test]
    fn snmp_credentials_come_with_the_scan() {
        let mut v1 = check(10, "161");
        v1.community = "{$SNMP_COMMUNITY}".into();
        let mut v3 = check(13, "1161");
        v3.security_name = "monitor".into();
        v3.security_level = 2;
        v3.auth_protocol = 3;
        v3.auth_passphrase = "auth-secret".into();
        v3.priv_protocol = 1;
        v3.priv_passphrase = "priv-secret".into();
        let mut des = v3.clone();
        des.priv_protocol = 0;
        let config = Config {
            discovery: vec![
                rule("10.0.0.1", "1h", vec![v1, check(12, "")]),
                rule("10.0.0.1", "1h", vec![v3]),
                rule("10.0.0.1", "1h", vec![des]),
            ],
            global_macros: vec![Macro {
                name: "{$SNMP_COMMUNITY}".into(),
                value: Some("public".into()),
            }],
            ..Config::default()
        };
        let out = read(&config);
        let v2c = out.scans[0].snmp.as_ref().unwrap();
        assert_eq!((v2c.version.as_str(), v2c.port), ("v2c", 161));
        assert_eq!(v2c.community.as_ref().unwrap().expose(), "public");
        assert_eq!(note(&out.scans[0]), "zabbix_snmp_old");
        assert_eq!(out.scans[0].ports.as_deref(), Some(&[][..]));
        let v3 = out.scans[1].snmp.as_ref().unwrap();
        assert_eq!(
            (
                v3.port,
                v3.auth_protocol.as_deref(),
                v3.priv_protocol.as_deref()
            ),
            (1161, Some("sha256"), Some("aes128"))
        );
        assert_eq!(v3.priv_password.as_ref().unwrap().expose(), "priv-secret");
        assert_eq!(note(&out.scans[1]), "exact");
        assert!(out.scans[2].snmp.is_none());
        assert_eq!(note(&out.scans[2]), "zabbix_snmp_protocol");
        // Never in what the preview shows.
        let shown = serde_json::to_string(&out).unwrap();
        assert!(
            !shown.contains("secret") && !shown.contains("public"),
            "{shown}"
        );
    }

    #[test]
    fn auto_registration_cannot_be_moved() {
        let config = Config {
            actions: vec![Action {
                name: "Register Linux agents".into(),
                source: ActionSource::Registration,
                enabled: true,
                rules: vec![],
                adds_hosts: true,
            }],
            ..Config::default()
        };
        let out = read(&config);
        assert_eq!(out.unmovable[0].origin.kind, "action");
        assert_eq!(out.unmovable[0].why.code, "zabbix_autoregistration");
    }
}
